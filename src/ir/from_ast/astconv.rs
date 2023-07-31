//! Convert the frontend AST to the IR.
use super::build_ctx::InvPort;
use super::{BuildCtx, Sig, SigMap};
use crate::ir::{
    Cmp, Ctx, EventIdx, ExprIdx, MutCtx, ParamIdx, PortIdx, PropIdx, TimeIdx,
};
use crate::utils::GPosIdx;
use crate::{ast, ir, utils::Binding};
use itertools::Itertools;
use std::collections::HashMap;
use std::{iter, rc::Rc};

/// # Declare phase
/// This is the first pass over the AST and responsible for forward declaring names defined by invocations.
/// We do this because invocation ports can be used before their definition:
/// ```
/// p = new Prev[32]<G>(add.out);
/// add = new Add[32]<G>(p.out, 1);
/// ```
///
/// At a high-level, it does the following:
/// * Define all the locally-bound parameters, specifically those defined by loops.
///   This is needed to correctly resolve the instances which may use parameters.
/// * For each instance in the program, compute the partially resolved component
///   signature which substitutes all parameters in the signature.
/// * For each invocation, compute the fully resolved signature (where events are correctly substituted)
///   and define all the parameters.
impl<'ctx, 'prog> BuildCtx<'ctx, 'prog> {
    fn declare_inst(&mut self, inst: &ast::Instance) {
        let ast::Instance {
            name,
            component,
            bindings,
        } = inst;
        let comp = self.sigs.get(component).unwrap();
        let binding = self.param_binding(
            comp.params.clone(),
            bindings.iter().map(|e| e.inner()).cloned().collect_vec(),
        );
        let inst = ir::Instance {
            comp: comp.idx,
            params: binding
                .iter()
                .map(|(_, b)| self.expr(b.clone()))
                .collect_vec()
                .into_boxed_slice(),
            info: self.comp.add(ir::Info::instance(
                name.copy(),
                component.pos(),
                name.pos(),
            )),
        };
        let idx = self.comp.add(inst);
        self.inst_map.insert(name.copy(), idx);
        // Track the component binding for this instance
        self.inst_to_sig.push(idx, (Rc::new(binding), **component));
    }

    /// Invokes are the most complicated construct to compile. This function:
    /// 1. Creates a new invoke in the component with the time bindings.
    /// 2. Resolves output ports and defines them in the component
    fn declare_inv(&mut self, inv: &ast::Invoke) {
        let ast::Invoke {
            name,
            instance,
            abstract_vars,
            ..
        } = inv;
        let inst = *self.inst_map.get(instance).unwrap();
        let info = self.comp.add(ir::Info::invoke(
            name.copy(),
            instance.pos(),
            name.pos(),
        ));
        let inv = self.comp.add(ir::Invoke {
            inst,
            ports: vec![],  // Filled in later
            events: vec![], // Filled in later
            info,
        });
        // foreign component being invoked
        let foreign_comp = inv.comp(self.comp);
        self.add_inv(name.copy(), inv);

        let mut def_ports = vec![];

        // The inputs
        let (param_binding, comp) = self.inst_to_sig.get(inst).clone();
        let sig = self.sigs.get(&comp).unwrap();

        // Event bindings
        let event_binding = self.event_binding(
            sig.events.clone(),
            abstract_vars.iter().map(|v| v.inner().clone()),
        );

        // Define the output port from the invoke
        for (idx, p) in sig.outputs.clone().into_iter().enumerate() {
            let resolved = p
                .resolve_exprs(&param_binding)
                .resolve_event(&event_binding);

            let base = ir::Foreign::new(
                self.ctx
                    .comps
                    .get(foreign_comp)
                    .outputs()
                    .nth(idx)
                    .unwrap()
                    .0,
                foreign_comp,
            );

            let owner = ir::PortOwner::Inv {
                inv,
                dir: ir::Direction::Out,
                base,
            };
            def_ports.push(self.port(resolved, owner));
        }

        // Add the inputs from the invoke. The outputs are added in the second
        // pass over the AST.
        self.comp.get_mut(inv).ports.extend(def_ports);
    }

    /// Declare the instances and invokes in the current scope.
    /// This does not burrow into inner scopes.
    fn declare_cmd(&mut self, cmd: &ast::Command) {
        match cmd {
            ast::Command::Instance(inst) => {
                self.declare_inst(inst);
            }
            ast::Command::Invoke(inv) => {
                self.declare_inv(inv);
            }
            ast::Command::ForLoop(_)
            | ast::Command::If(_)
            | ast::Command::Fact(_)
            | ast::Command::Connect(_)
            | ast::Command::Bundle(_) => {}
        }
    }

    fn declare_cmds(&mut self, cmds: &[ast::Command]) {
        for cmd in cmds {
            self.declare_cmd(cmd);
        }
    }
}

impl<'ctx, 'prog> BuildCtx<'ctx, 'prog> {
    fn expr(&mut self, expr: ast::Expr) -> ExprIdx {
        match expr {
            ast::Expr::Abstract(p) => {
                let Some(pidx) = self.get_param(&p) else {
                    unreachable!("Parameter {p} not found")
                };
                self.comp.add(ir::Expr::Param(pidx))
            }
            ast::Expr::Concrete(n) => {
                let e = ir::Expr::Concrete(n);
                self.comp.add(e)
            }
            ast::Expr::Op { op, left, right } => {
                let l = self.expr(*left);
                let r = self.expr(*right);
                match op {
                    ast::Op::Add => l.add(r, self.comp),
                    ast::Op::Mul => l.mul(r, self.comp),
                    ast::Op::Sub => l.sub(r, self.comp),
                    ast::Op::Div => l.div(r, self.comp),
                    ast::Op::Mod => l.rem(r, self.comp),
                }
            }
            ast::Expr::App { func, arg } => {
                let arg = self.expr(*arg);
                match func {
                    ast::UnFn::Pow2 => arg.pow2(self.comp),
                    ast::UnFn::Log2 => arg.log2(self.comp),
                }
            }
        }
    }

    fn expr_cons(&mut self, cons: ast::OrderConstraint<ast::Expr>) -> PropIdx {
        let lhs = self.expr(cons.left);
        let rhs = self.expr(cons.right);
        let op = match cons.op {
            ast::OrderOp::Gt => Cmp::Gt,
            ast::OrderOp::Gte => Cmp::Gte,
            ast::OrderOp::Eq => Cmp::Eq,
        };
        self.comp.add(ir::Prop::Cmp(ir::CmpOp { lhs, op, rhs }))
    }

    fn event_cons(&mut self, cons: ast::OrderConstraint<ast::Time>) -> PropIdx {
        let lhs = self.time(cons.left);
        let rhs = self.time(cons.right);
        let op = match cons.op {
            ast::OrderOp::Gt => Cmp::Gt,
            ast::OrderOp::Gte => Cmp::Gte,
            ast::OrderOp::Eq => Cmp::Eq,
        };
        self.comp.add(ir::Prop::TimeCmp(ir::CmpOp { lhs, op, rhs }))
    }

    fn implication(&mut self, i: ast::Implication<ast::Expr>) -> PropIdx {
        let cons = self.expr_cons(i.cons);
        if let Some(ante) = i.guard {
            let ante = self.expr_cons(ante);
            ante.implies(cons, self.comp)
        } else {
            cons
        }
    }

    /// Add a parameter to the component.
    fn param(
        &mut self,
        param: &ast::ParamBind,
        owner: ir::ParamOwner,
    ) -> ParamIdx {
        let info = self.comp.add(ir::Info::param(param.name(), param.pos()));

        let ir_param = ir::Param::new(owner, info);
        let is_sig_param = ir_param.is_sig_owned();

        let idx = self.comp.add(ir_param);
        self.add_param(param.name(), idx);

        // only add information if this is a signature defined parameter
        if is_sig_param {
            // If the component is expecting interface information, add it.
            if let Some(src) = &mut self.comp.src_info {
                src.params.insert(idx, param.name());
            }
        }

        idx
    }

    fn time(&mut self, t: ast::Time) -> TimeIdx {
        let Some(event) = self.event_map.get(&t.event).copied() else {
            unreachable!("Event {} not found. Map:\n{}", t.event, self.event_map)
        };
        let offset = self.expr(t.offset);
        self.comp.add(ir::Time { event, offset })
    }

    fn timesub(&mut self, ts: ast::TimeSub) -> ir::TimeSub {
        match ts {
            ast::TimeSub::Unit(e) => ir::TimeSub::Unit(self.expr(e)),
            ast::TimeSub::Sym { l, r } => {
                let l = self.time(l);
                let r = self.time(r);
                l.sub(r, self.comp)
            }
        }
    }

    /// Forward declare an event without adding its delay. We need to do this
    /// since delays of events may mention the event itself.
    /// `interface_port` is the optional interface port associated with this event.
    fn declare_event(
        &mut self,
        eb: &ast::EventBind,
        interface_port: Option<(ast::Id, GPosIdx)>,
    ) -> EventIdx {
        let info = self.comp.add(ir::Info::event(
            eb.event.copy(),
            eb.event.pos(),
            eb.delay.pos(),
            interface_port,
        ));
        // Add a fake delay of 0.
        let e = ir::Event {
            delay: self.comp.num(0).into(),
            info,
            has_interface: interface_port.is_some(),
        };
        let idx = self.comp.add(e);

        // If the component is expecting interface information and there is an interface port, add it.
        if let (Some((name, _)), Some(src)) =
            (interface_port, &mut self.comp.src_info)
        {
            src.interface_ports.insert(idx, name);
        }

        log::trace!("Added event {} as {idx}", eb.event);
        self.event_map.insert(*eb.event, idx);
        idx
    }

    fn range(&mut self, r: ast::Range) -> ir::Range {
        let start = self.time(r.start);
        let end = self.time(r.end);
        ir::Range { start, end }
    }

    fn port(&mut self, pd: ast::PortDef, owner: ir::PortOwner) -> PortIdx {
        let (name, p) = match pd {
            ast::PortDef::Port {
                name,
                liveness,
                bitwidth,
            } => {
                let info = self.comp.add(ir::Info::port(
                    name.copy(),
                    name.pos(),
                    bitwidth.pos(),
                    liveness.pos(),
                ));

                // The bundle type uses a fake bundle index and has a length of 1.
                // We don't need to push a new scope because this type is does not
                // bind any new parameters.
                let p_name = self.gen_name();
                let live = self.with_scope(|ctx| ir::Liveness {
                    idx: ctx.param(
                        &ast::ParamBind::from(p_name),
                        // Updated after the port is constructed
                        ir::ParamOwner::bundle(ir::PortIdx::UNKNOWN),
                    ), // This parameter is unused
                    len: ctx.comp.num(1),
                    range: ctx.range(liveness.take()),
                });
                let p = ir::Port {
                    width: self.expr(bitwidth.take()),
                    owner,
                    live,
                    info,
                };
                (name, p)
            }
            ast::PortDef::Bundle(ast::Bundle {
                name,
                typ:
                    ast::BundleType {
                        idx,
                        len,
                        liveness,
                        bitwidth,
                    },
            }) => {
                let info = self.comp.add(ir::Info::port(
                    name.copy(),
                    name.pos(),
                    bitwidth.pos(),
                    liveness.pos(),
                ));
                // Construct the bundle type in a new scope.
                let live = self.with_scope(|ctx| ir::Liveness {
                    idx: ctx.param(
                        // Updated after the port is constructed
                        &ast::ParamBind::from(idx),
                        ir::ParamOwner::bundle(PortIdx::UNKNOWN),
                    ),
                    len: ctx.expr(len.take()),
                    range: ctx.range(liveness.take()),
                });
                let p = ir::Port {
                    width: self.expr(bitwidth.take()),
                    owner,
                    live,
                    info,
                };
                (name, p)
            }
        };

        // Defines helper variable here due to lifetime issues
        let is_sig_port = p.is_sig();
        let idx = self.comp.add(p);
        // Fixup the liveness index parameter's owner
        let p = self.comp.get(idx);
        let param = self.comp.get_mut(p.live.idx);
        param.owner = ir::ParamOwner::bundle(idx);

        // If this is a signature port, try adding it to the component's external interface
        if is_sig_port {
            // If the component is expecting interface information, add it.
            if let Some(src) = &mut self.comp.src_info {
                src.ports.insert(idx, name.copy());
            }
        }

        // Add the port to the current scope
        self.add_port(*name, idx);

        idx
    }

    /// Transforms an access into (start, end)
    fn access(&mut self, access: ast::Access) -> (ir::ExprIdx, ir::ExprIdx) {
        match access {
            ast::Access::Index(n) => {
                let n = self.expr(n);
                (n, n.add(self.comp.num(1), self.comp))
            }
            ast::Access::Range { start, end } => {
                (self.expr(start), self.expr(end))
            }
        }
    }

    /// Get the index associated with an AST port. The port must have been
    /// previously defined.
    fn get_access(
        &mut self,
        port: ast::Port,
        dir: ir::Direction,
    ) -> ir::Access {
        match port {
            ast::Port::This(n) => {
                let owner = InvPort::Sig(dir, n.copy());
                ir::Access::port(self.get_port(&owner), self.comp)
            }
            ast::Port::InvPort { invoke, name } => {
                let inv = self.get_inv(invoke.copy());
                let owner = InvPort::Inv(inv, dir, name.copy());
                ir::Access::port(self.get_port(&owner), self.comp)
            }
            ast::Port::Bundle { name, access } => {
                // NOTE(rachit): The AST does not distinguish between bundles
                // defined by the signature and locally defined bundles so we
                // must search both.
                let owner = InvPort::Sig(dir, name.copy());
                let port = if let Some(p) = self.find_port(&owner) {
                    p
                } else {
                    let owner = InvPort::Local(name.copy());
                    self.get_port(&owner)
                };
                let (start, end) = self.access(access.take());
                ir::Access { port, start, end }
            }
            ast::Port::InvBundle {
                invoke,
                port,
                access,
            } => {
                let inv = self.get_inv(invoke.copy());
                let owner = InvPort::Inv(inv, dir, port.copy());
                let port = self.get_port(&owner);
                let (start, end) = self.access(access.take());
                ir::Access { port, start, end }
            }
            ast::Port::Constant(_) => todo!("Constant ports"),
        }
    }

    fn sig(&mut self, sig: ast::Signature) -> Vec<ir::Command> {
        for param in &sig.params {
            self.param(param.inner(), ir::ParamOwner::Sig);
        }
        let mut interface_signals: HashMap<_, _> = sig
            .interface_signals
            .iter()
            .cloned()
            .map(|ast::InterfaceDef { name, event }| (event, name.split()))
            .collect();
        // Declare the events first
        for event in &sig.events {
            // can remove here as each interface signal should only be used once
            let interface = interface_signals.remove(event.event.inner());
            self.declare_event(event.inner(), interface);
        }
        // Then define their delays correctly
        for event in &sig.events {
            let delay = self.timesub(event.inner().delay.inner().clone());
            let idx = self.event_map.get(&event.inner().event).unwrap();
            self.comp.get_mut(*idx).delay = delay;
        }
        for port in sig.inputs() {
            // XXX(rachit): Unnecessary clone.
            self.port(port.inner().clone(), ir::PortOwner::sig_out());
        }
        for port in sig.outputs() {
            // XXX(rachit): Unnecessary clone.
            self.port(port.inner().clone(), ir::PortOwner::sig_in());
        }
        for (name, width) in sig.unannotated_ports {
            self.comp.unannotated_ports.push((name, width));
        }
        // Constraints defined by the signature
        let mut cons = Vec::with_capacity(
            sig.param_constraints.len() + sig.event_constraints.len(),
        );
        for ec in sig.event_constraints {
            let info = self.comp.add(ir::Info::assert(ir::info::Reason::misc(
                "Signature assumption",
                ec.pos(),
            )));
            let prop = self.event_cons(ec.take());
            cons.extend(self.comp.assume(prop, info));
        }
        for pc in sig.param_constraints {
            let info = self.comp.add(ir::Info::assert(ir::info::Reason::misc(
                "Signature assumption",
                pc.pos(),
            )));
            let prop = self.expr_cons(pc.take());
            cons.extend(self.comp.assume(prop, info));
        }

        cons
    }

    fn instance(&mut self, inst: ast::Instance) -> Vec<ir::Command> {
        let comp_loc = inst.component.pos();
        // Add the facts defined by the instance as assertions in the
        // component.
        let idx = *self.inst_map.get(&inst.name).unwrap();
        let (binding, component) = self.inst_to_sig.get(idx).clone();
        let facts = self
            .sigs
            .get(&component)
            .unwrap()
            .param_cons
            .clone()
            .into_iter()
            .flat_map(|f| {
                let reason = self.comp.add(
                    ir::info::Reason::param_cons(comp_loc, f.pos()).into(),
                );
                let p = f.take().resolve_expr(&binding);
                let prop = self.expr_cons(p);
                // This is a checked fact because the calling component needs to
                // honor it.
                self.comp.assert(prop, reason)
            })
            .collect_vec();

        iter::once(ir::Command::from(idx))
            .chain(facts.into_iter())
            .collect_vec()
    }

    /// Construct an event binding from this Signature's events and the given
    /// arguments.
    /// Fills in the missing arguments with default values
    pub fn event_binding(
        &self,
        events: impl IntoIterator<Item = ast::EventBind>,
        args: impl IntoIterator<Item = ast::Time>,
    ) -> Binding<ast::Time> {
        let args = args.into_iter().collect_vec();
        let events = events.into_iter().collect_vec();
        assert!(
            events.iter().take_while(|ev| ev.default.is_none()).count()
                <= args.len(),
            "Insuffient events for component invocation.",
        );

        let mut partial_map = Binding::new(
            events
                .iter()
                .map(|eb| eb.event.inner())
                .cloned()
                .zip(args.iter().cloned()),
        );
        // Skip the events that have been bound
        let remaining = events
            .iter()
            .skip(args.len())
            .map(|eb| {
                let bind = eb
                    .default
                    .as_ref()
                    .unwrap()
                    .clone()
                    .resolve_event(&partial_map);
                (*eb.event.inner(), bind)
            })
            .collect();

        partial_map.extend(remaining);
        partial_map
    }

    /// Construct a param binding from this Signature's parameters and the given
    /// arguments.
    /// Fills in the missing arguments with default values
    pub fn param_binding(
        &self,
        params: impl IntoIterator<Item = ast::ParamBind>,
        args: impl IntoIterator<Item = ast::Expr>,
    ) -> Binding<ast::Expr> {
        let args = args.into_iter().collect_vec();
        let params = params.into_iter().collect_vec();
        assert!(
            params.iter().take_while(|ev| ev.default.is_none()).count()
                <= args.len(),
            "Insuffient params for component invocation.",
        );

        let mut partial_map = Binding::new(
            params.iter().map(|pb| pb.name()).zip(args.iter().cloned()),
        );
        // Skip the events that have been bound
        let remaining = params
            .iter()
            .skip(args.len())
            .map(|pb| {
                let bind =
                    pb.default.as_ref().unwrap().clone().resolve(&partial_map);
                (pb.name(), bind)
            })
            .collect();

        partial_map.extend(remaining);
        partial_map
    }

    /// This function is called during the second pass of the conversion and does the following:
    /// * Defines the input ports of the invocation
    /// * Generate event bindings implied by the invocation
    /// * Generates the connections implied by the arguments to the invoke
    fn invoke(&mut self, inv: ast::Invoke) -> Vec<ir::Command> {
        let ast::Invoke {
            name,
            abstract_vars,
            ports,
            instance,
        } = inv;
        let Some(ports) = ports else {
            unreachable!("No ports provided for invocation {name}")
        };
        let inv = self.get_inv(name.copy());
        let inst = inv.inst(self.comp);
        let (param_binding, comp) = self.inst_to_sig.get(inst).clone();
        let sig = self.sigs.get(&comp).unwrap();
        // foreign component being invoked
        let foreign_comp = inv.comp(self.comp);

        // Event bindings
        let event_binding = self.event_binding(
            sig.events.iter().cloned(),
            abstract_vars.iter().map(|v| v.inner().clone()),
        );

        let srcs = ports
            .into_iter()
            .map(|p| p.map(|p| self.get_access(p, ir::Direction::Out)))
            .collect_vec();
        assert!(
            sig.inputs.len() == srcs.len(),
            "signature defined {} inputs but provided {} arguments",
            sig.inputs.len(),
            srcs.len()
        );

        // Constraints on the events from the signature
        let cons: Vec<ir::Command> = sig
            .event_cons
            .clone()
            .into_iter()
            .flat_map(|ec| {
                let reason = self.comp.add(
                    ir::info::Reason::event_cons(instance.pos(), ec.pos())
                        .into(),
                );
                let ec = ec.take().resolve_event(&event_binding);
                let prop = self.event_cons(ec);
                self.comp.assert(prop, reason)
            })
            .collect();

        let mut connects = Vec::with_capacity(sig.inputs.len());

        for (idx, (p, src)) in
            sig.inputs.clone().into_iter().zip(srcs).enumerate()
        {
            let info = self
                .comp
                .add(ir::Info::connect(p.inner().name().pos(), src.pos()));
            let resolved = p.map(|p| {
                p.resolve_exprs(&param_binding)
                    .resolve_event(&event_binding)
            });

            let base = ir::Foreign::new(
                self.ctx
                    .comps
                    .get(foreign_comp)
                    .inputs()
                    .nth(idx)
                    .unwrap()
                    .0,
                foreign_comp,
            );

            let owner = ir::PortOwner::Inv {
                inv,
                dir: ir::Direction::In,
                base,
            };

            // Define port and add it to the invocation
            let pidx = self.port(resolved.take(), owner);
            self.comp.get_mut(inv).ports.push(pidx);

            let end = self.comp[pidx].live.len;
            let dst = ir::Access {
                port: pidx,
                start: self.comp.num(0),
                end,
            };
            connects.push(
                ir::Connect {
                    src: src.take(),
                    dst,
                    info,
                }
                .into(),
            )
        }

        // Events defined by the invoke
        sig.events
            .iter()
            .zip_longest(abstract_vars.iter())
            .map(|pair| match pair {
                itertools::EitherOrBoth::Both(evt, t) => {
                    (evt, t.inner(), t.pos())
                }
                itertools::EitherOrBoth::Left(evt) => (
                    evt,
                    event_binding.get(evt.event.inner()),
                    GPosIdx::UNKNOWN,
                ),
                itertools::EitherOrBoth::Right(_) => {
                    unreachable!("More arguments than events.")
                }
            })
            .enumerate()
            .for_each(|(idx, (event, time, pos))| {
                let ev_delay_loc = event.delay.pos();
                let resolved = event
                    .clone()
                    .resolve_exprs(&param_binding)
                    .resolve_event(&event_binding);

                let info =
                    self.comp.add(ir::Info::event_bind(ev_delay_loc, pos));
                let arg = self.time(time.clone());
                let event = self.timesub(resolved.delay.take());
                let base = ir::Foreign::new(EventIdx::new(idx), foreign_comp);
                let eb = ir::EventBind::new(event, arg, info, base);
                let invoke = self.comp.get_mut(inv);
                invoke.events.push(eb);
            });

        std::iter::once(ir::Command::from(inv))
            .chain(connects)
            .chain(cons)
            .collect_vec()
    }

    fn commands(&mut self, cmds: Vec<ast::Command>) -> Vec<ir::Command> {
        self.declare_cmds(&cmds);
        cmds.into_iter().flat_map(|c| self.command(c)).collect_vec()
    }

    fn command(&mut self, cmd: ast::Command) -> Vec<ir::Command> {
        match cmd {
            ast::Command::Invoke(inv) => self.invoke(inv),
            ast::Command::Instance(inst) => self.instance(inst),
            ast::Command::Fact(ast::Fact { cons, checked }) => {
                let reason = self.comp.add(
                    ir::info::Reason::misc("source-level fact", cons.pos())
                        .into(),
                );
                let prop = self.implication(cons.take());
                let fact = if checked {
                    self.comp.assert(prop, reason)
                } else {
                    self.comp.assume(prop, reason)
                };
                fact.into_iter().collect()
            }
            ast::Command::Connect(ast::Connect { src, dst, guard }) => {
                assert!(guard.is_none(), "Guards are not supported");
                let info =
                    self.comp.add(ir::Info::connect(dst.pos(), src.pos()));
                let src = self.get_access(src.take(), ir::Direction::Out);
                let dst = self.get_access(dst.take(), ir::Direction::In);
                vec![ir::Connect { src, dst, info }.into()]
            }
            ast::Command::ForLoop(ast::ForLoop {
                idx,
                start,
                end,
                body,
            }) => {
                let start = self.expr(start);
                let end = self.expr(end);
                // Assumption that the index is within range
                let reason = self.comp.add(
                    ir::info::Reason::misc(
                        "loop index is within range",
                        idx.pos(),
                    )
                    .into(),
                );

                // Compile the body in a new scope
                let (index, body) = self.with_scope(|this| {
                    let idx = this.param(
                        &ast::ParamBind::from(idx),
                        ir::ParamOwner::Loop,
                    );
                    (idx, this.commands(body))
                });
                let l = ir::Loop {
                    index,
                    start,
                    end,
                    body,
                }
                .into();
                let index = index.expr(self.comp);
                let idx_start = index.gte(start, self.comp);
                let idx_end = index.lt(end, self.comp);
                let in_range = idx_start.and(idx_end, self.comp);
                iter::once(l)
                    .chain(self.comp.assume(in_range, reason))
                    .collect()
            }
            ast::Command::If(ast::If { cond, then, alt }) => {
                let cond = self.expr_cons(cond);
                let then = self.commands(then);
                let alt = self.commands(alt);
                vec![ir::If { cond, then, alt }.into()]
            }
            ast::Command::Bundle(bun) => {
                // Add the bundle to the current scope
                self.port(ast::PortDef::Bundle(bun), ir::PortOwner::Local);
                vec![]
            }
        }
    }

    /// Adds assumptions about the ports in the component
    fn port_assumptions(&mut self) -> Vec<ir::Command> {
        let mut cmds = Vec::with_capacity(self.comp.ports().len() * 2);
        let ports = self
            .comp
            .ports()
            .iter()
            .map(|(_, p)| (p.live.idx, p.live.len))
            .collect_vec();
        // Add assumptions for range of bundle-bound indices
        let reason = self.comp.add(
            ir::info::Reason::misc(
                "bundle index is within range",
                GPosIdx::UNKNOWN,
            )
            .into(),
        );
        for (idx, len) in ports {
            let idx = idx.expr(self.comp);
            let start = idx.gte(self.comp.num(0), self.comp);
            let end = idx.lt(len, self.comp);
            let in_range = start.and(end, self.comp);
            cmds.extend(self.comp.assume(in_range, reason))
        }
        cmds
    }

    fn external(ctx: &ir::Context, sig: ast::Signature) -> ir::Component {
        let mut ir_comp = ir::Component::new(true);
        ir_comp.src_info = Some(ir::InterfaceSrc::new(sig.name.copy()));
        let binding = SigMap::default();
        let mut builder = BuildCtx::new(ctx, &mut ir_comp, &binding);

        // First we declare all the ports
        let mut cmds = builder.sig(sig);
        cmds.extend(builder.port_assumptions());
        ir_comp.cmds = cmds;
        ir_comp
    }

    fn comp(
        ctx: &ir::Context,
        ir_comp: &mut ir::Component,
        comp: ast::Component,
        sigs: &'prog SigMap,
    ) {
        let mut builder = BuildCtx::new(ctx, ir_comp, sigs);

        let mut cmds = builder.sig(comp.sig);
        let body_cmds = builder.commands(comp.body);
        cmds.extend(builder.port_assumptions());
        cmds.extend(body_cmds);
        ir_comp.cmds = cmds;
    }
}

pub fn transform(ns: ast::Namespace) -> ir::Context {
    let mut sig_map = SigMap::default();
    let main_idx = ns.main_idx();
    let (mut ns, order) = crate::utils::Traversal::from(ns).take();

    // Walk over signatures and build a SigMap
    for (idx, sig) in ns.signatures().map(|(_, sig)| sig).enumerate() {
        sig_map.insert(sig.name.copy(), Sig::from((sig, idx)));
    }

    let mut ctx = ir::Context::default();
    for (file, exts) in ns.externs {
        for ext in exts {
            let idx = sig_map.get(&ext.name).unwrap().idx;
            log::debug!("Converting external {}: {}", ext.name, idx);
            let ir_ext = BuildCtx::external(&ctx, ext);
            ctx.comps.checked_add(idx, ir_ext);
            ctx.externals.entry(file.clone()).or_default().push(idx);
        }
    }

    // TODO: Need to handle recursive components as well as mutually recursive components by adding signature ports first in declaration, so that foreign keys can be resolved
    // declare all components in the proper order
    for (cidx, comp) in ns.components.iter().enumerate() {
        let idx = sig_map.get(&comp.sig.name).unwrap().idx;

        let mut ir_comp = ir::Component::new(false);
        if Some(cidx) == main_idx {
            ctx.entrypoint = Some(idx);
            ir_comp.src_info =
                Some(ir::InterfaceSrc::new(comp.sig.name.copy()));
        }
        ctx.comps.checked_add(idx, ir_comp);
    }

    // create a dummy component to be swapped into the context
    let mut curr_comp = ir::Component::new(false);
    for cidx in order {
        let comp = std::mem::take(&mut ns.components[cidx]);
        log::debug!("Compiling component {}", comp.sig.name);
        let idx = sig_map.get(&comp.sig.name).unwrap().idx;

        // Needs to swap here because we have to own the current component while leaving the context immutable.
        // TODO: Find a solution here that will still allow us to properly generate Foreign keys in recursive components.
        std::mem::swap(ctx.get_mut(idx), &mut curr_comp);
        BuildCtx::comp(&ctx, &mut curr_comp, comp, &sig_map);
        // swap the component back into place
        std::mem::swap(ctx.get_mut(idx), &mut curr_comp);
    }

    ctx
}