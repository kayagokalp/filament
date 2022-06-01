use std::rc::Rc;

use itertools::Itertools;

use crate::{core, errors::FilamentResult};

/// The type ascribed to an interval time expression
#[derive(PartialEq)]
enum EvType {
    Event,
    Nat,
}

/// Checks that time specification follow the simple type system:
/// event   :: T
/// +       :: event -> nat -> event
/// max     :: event -> event -> event
fn type_check(it: &core::IntervalTime) -> EvType {
    match it {
        core::IntervalTime::Abstract(_) => EvType::Event,
        core::IntervalTime::Concrete(_) => EvType::Nat,
        core::IntervalTime::Add { left, right } => {
            match (type_check(left), type_check(right)) {
                (EvType::Event, EvType::Nat) | (EvType::Nat, EvType::Event) => {
                    EvType::Event
                }
                _ => panic!("Unexpected type for add expression"),
            }
        }
        core::IntervalTime::Max { left, right } => {
            match (type_check(left), type_check(right)) {
                (EvType::Event, EvType::Event) => EvType::Event,
                _ => panic!("Unexpected type for max expression"),
            }
        }
    }
}

fn transform_time(it: core::IntervalTime) -> core::FsmIdxs {
    assert!(
        type_check(&it) == EvType::Event,
        "interval time does not represent a valid event"
    );
    it.into()
}

fn transform_range(
    range: core::Range<core::IntervalTime>,
) -> core::Range<core::FsmIdxs> {
    core::Range {
        start: range.start.into(),
        end: range.end.into(),
    }
}

fn transform_interval(
    interval: core::Interval<core::IntervalTime>,
) -> core::Interval<core::FsmIdxs> {
    core::Interval {
        within: transform_range(interval.within),
        exact: interval.exact.map(transform_range),
    }
}

fn transform_control(
    con: core::Command<core::IntervalTime>,
) -> FilamentResult<core::Command<core::FsmIdxs>> {
    match con {
        core::Command::Invoke(core::Invoke {
            bind,
            rhs:
                core::Invocation {
                    comp,
                    abstract_vars,
                    ports,
                    ..
                },
        }) => {
            let abs: Vec<core::FsmIdxs> =
                abstract_vars.into_iter().map(transform_time).collect();
            let rhs = core::Invocation::new(comp, abs, ports);
            Ok(core::Command::Invoke(core::Invoke { bind, rhs }))
        }
        core::Command::When(core::When { commands, time }) => {
            Ok(core::Command::when(
                transform_time(time),
                commands
                    .into_iter()
                    .map(transform_control)
                    .collect::<FilamentResult<Vec<_>>>()?,
            ))
        }
        core::Command::Instance(ins) => Ok(core::Command::Instance(ins)),
        core::Command::Connect(con) => Ok(core::Command::Connect(con)),
    }
}

fn transform_port_def(
    pd: core::PortDef<core::IntervalTime>,
) -> core::PortDef<core::FsmIdxs> {
    core::PortDef {
        liveness: transform_interval(pd.liveness),
        name: pd.name,
        bitwidth: pd.bitwidth,
    }
}

fn transform_constraints(
    con: core::Constraint<core::IntervalTime>,
) -> core::Constraint<core::FsmIdxs> {
    core::Constraint {
        left: transform_time(con.left),
        right: transform_time(con.right),
        op: con.op,
    }
}

fn transform_signature(
    sig: core::Signature<core::IntervalTime>,
) -> core::Signature<core::FsmIdxs> {
    core::Signature {
        inputs: sig.inputs.into_iter().map(transform_port_def).collect(),
        outputs: sig.outputs.into_iter().map(transform_port_def).collect(),
        constraints: sig
            .constraints
            .into_iter()
            .map(transform_constraints)
            .collect(),
        name: sig.name,
        abstract_vars: sig.abstract_vars,
        interface_signals: sig.interface_signals,
    }
}

pub fn check_and_transform(
    ns: core::Namespace<core::IntervalTime>,
) -> FilamentResult<core::Namespace<core::FsmIdxs>> {
    let components = ns
        .components
        .into_iter()
        .map(|comp| {
            let commands = comp
                .body
                .into_iter()
                .map(transform_control)
                .collect::<FilamentResult<Vec<_>>>()?;

            Ok(core::Component::new(
                transform_signature(Rc::try_unwrap(comp.sig).unwrap()),
                commands,
            ))
        })
        .collect::<FilamentResult<Vec<_>>>()?;

    Ok(core::Namespace {
        imports: ns.imports,
        signatures: ns
            .signatures
            .into_iter()
            .map(transform_signature)
            .collect_vec(),
        components,
    })
}