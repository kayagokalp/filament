use itertools::Itertools;

use super::{Context, THIS};
use crate::{
    core,
    errors::{Error, FilamentResult},
};

type Intervals = Vec<core::Interval<super::TimeRep>>;

/// Computes the availablity of guards.
fn guard_availability(
    guard: &core::Guard,
    ctx: &Context,
    acc: &mut Intervals,
) -> FilamentResult<()> {
    match guard {
        core::Guard::Or(g1, g2) => {
            guard_availability(g1, ctx, acc)?;
            guard_availability(g2, ctx, acc)?;
            Ok(())
        }
        core::Guard::Port(p) => {
            let interval = match p {
                core::Port::ThisPort(name) => {
                    ctx.get_invoke(&THIS.into())?.port_guarantees(name)?
                }
                core::Port::CompPort { comp, name } => {
                    ctx.get_invoke(comp)?.port_guarantees(name)?
                }
                core::Port::Constant(_) => {
                    return Err(Error::malformed(
                        "Guards cannot contain constants",
                    ))
                }
            };

            if interval.exact.is_none() {
                return Err(Error::malformed(
                    format!(
                        "Port does not have an exact guarantee `{}`. Ports used in guards must have exact guarantees",
                        p
                    ),
                ));
            }
            acc.push(interval);
            Ok(())
        }
    }
}

/// Merges exactly available signals into one interval.
/// All intervals need to be based on the same time variable and only be increments.
fn merge_availability(intervals: Intervals) -> core::Interval<super::TimeRep> {
    assert!(
        !intervals.is_empty(),
        "Cannot compute availablity with empty intervals"
    );

    // Check that all within intervals are the same.
    if !intervals
        .iter()
        .map(|interval| &interval.within)
        .all_equal()
    {
        panic!("Intervals used in guards must be available for the same time")
    }
    let within = intervals[0].within.clone();

    let (event, start, end) = intervals
        .iter()
        .map(|iv| {
            iv.as_exact_offset().unwrap_or_else(|| {
                panic!("Cannot convert interval into offset: {:?}", iv)
            })
        })
        .sorted_by(|ev1, ev2| ev1.1.cmp(&ev2.1))
        .reduce(|(event, start, end), (ev, s, e)| {
            assert!(
                event == ev,
                "Intervals use different events: {event} and {ev}",
            );
            assert!(s == end, "Ranges are not continuous");
            (event, start, e)
        })
        .unwrap();

    core::Interval::new(within).with_exact(core::Range {
        start: core::FsmIdxs::unit(event.clone(), start),
        end: core::FsmIdxs::unit(event.clone(), end),
    })
}

pub fn total_interval(
    guard: &core::Guard,
    ctx: &Context,
) -> FilamentResult<core::Interval<super::TimeRep>> {
    let mut intervals = vec![];
    guard_availability(guard, ctx, &mut intervals)?;
    Ok(merge_availability(intervals))
}