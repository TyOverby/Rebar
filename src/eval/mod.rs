use std::vec::IntoIter;

use super::{Value, AresError, AresResult};

pub use self::environment::{Env, Environment};
pub use self::foreign_function::{ForeignFunction, free_fn, ast_fn, user_fn, FfType};
pub use self::procedure::{Procedure, ParamBinding};
pub use self::context::{Context, LoadedContext, State};

mod environment;
mod foreign_function;
mod procedure;
mod context;

pub enum StepState {
    EvalThis(Value),
    Return,
    Complete(Value),
    PreEvaluatedLambda {
        unevaluated: IntoIter<Value>
    },
    Lambda {
        func: Procedure,
        evaluated: Vec<Value>,
        unevaluated: IntoIter<Value>
    }
}

pub fn cleanup_stack<S: ?Sized + State>(prev_length: usize, ctx: &mut LoadedContext<S>) {
    while ctx.stack.len() > (prev_length - 2) {
        ctx.stack.pop();
    }
}

pub fn eval<S: ?Sized>(value: &Value, ctx: &mut LoadedContext<S>, proc_head: bool) -> AresResult<Value>
where S: State
{
    let value = value.clone();

    ctx.stack.push(StepState::Return);
    ctx.stack.push(StepState::EvalThis(value));

    let len = ctx.stack.len();
    if let Err(e) = step_eval(ctx, proc_head) {
        cleanup_stack(len, ctx);
        return Err(e);
    }

    while ctx.stack.len() > len {
        if let Err(e) = step_eval(ctx, proc_head) {
            cleanup_stack(len, ctx);
            return Err(e);
        }
    }

    let result = ctx.stack.pop();
    let ret = ctx.stack.pop();
    match (ret, result) {
        (Some(StepState::Return), Some(StepState::Complete(value))) => {
            Ok(value)
        }
        (res, r) => panic!("eval(..): invalid stack state [..., {:?}, {:?}]", r, res),
    }
}

fn step_eval<S: State + ?Sized>(ctx: &mut LoadedContext<S>, proc_head: bool) -> AresResult<()> {
    let top = ctx.stack.pop().unwrap();
    if let StepState::Complete(value) = top {
        match ctx.stack.pop().unwrap() {
            StepState::Lambda { func, mut evaluated, mut unevaluated } => {
                evaluated.push(value);
                if let Some(next) = unevaluated.next() {
                    ctx.stack.push(StepState::Lambda {
                        func: func,
                        evaluated: evaluated,
                        unevaluated: unevaluated,
                    });
                    ctx.stack.push(StepState::EvalThis(next));
                } else {
                    let returned = try!(apply_lambda(&func, evaluated, ctx));
                    ctx.stack.push(StepState::Complete(returned));
                }
            }
            a => panic!("step_eval(..): invalid stack state: [..., {:?}, {:?}]",
                        a, StepState::Complete(value))
        }
    } else {
        match top {
            StepState::EvalThis(value) => {
                try!(eval_this(value, ctx, proc_head));
            }
            StepState::Complete(_) => unreachable!(),
            a@StepState::Return |
            a@StepState::Lambda {..} |
            a@StepState::PreEvaluatedLambda { .. } =>
                panic!("step_eval(..): invalid stack state: [..., {:?}]", a)
        }
    }
    Ok(())
}

fn eval_this<S: State + ?Sized>(value: Value,
                               ctx: &mut LoadedContext<S>,
                               proc_head: bool)
                               -> AresResult<()> {
    match value {
        Value::Symbol(symbol) => {
            let func = ctx.env().borrow().get(symbol);
            match func {
                Some(Value::ForeignFn(ForeignFunction{typ: FfType::Ast, ..})) if !proc_head => {
                    Err(AresError::AstFunctionPass)
                }
                Some(v) => {
                    ctx.stack.push(StepState::Complete(v));
                    Ok(())
                },
                None => Err(AresError::UndefinedName(ctx.interner().lookup_or_anon(symbol))),
            }
        }

        Value::List(items) => {
            let mut args_count = items.len();
            let mut items = items.iter().cloned();
            let head = match items.next() {
                Some(h) => {
                    args_count -= 1;
                    h
                },
                None => return Err(AresError::ExecuteEmptyList),
            };
            let mut items: Vec<_> = items.collect();

            match try!(eval(&head, ctx, true)) {
                Value::Lambda(_, true) => Err(AresError::MacroReference),
                Value::Lambda(procedure, _) => {
                    if args_count == 0 {
                        let returned = try!(apply_lambda(&procedure, vec![], ctx));
                        ctx.stack.push(StepState::Complete(returned));
                        Ok(())
                    } else {
                        let evaluated = Vec::with_capacity(items.len());
                        let first = items.remove(0);
                        ctx.stack.push(StepState::Lambda {
                            func: procedure,
                            evaluated: evaluated ,
                            unevaluated: items.into_iter()
                        });
                        ctx.stack.push(StepState::EvalThis(first));
                        Ok(())
                    }
                }

                f@Value::ForeignFn(_) => {
                    let apply_result = try!(apply(&f, items, ctx));
                    ctx.stack.push(StepState::Complete(apply_result));
                    Ok(())

                }
                x => Err(AresError::UnexecutableValue(x)),
            }
        }

        Value::Lambda(_, true) => Err(AresError::MacroReference),

        v => {
            ctx.stack.push(StepState::Complete(v));
            Ok(())
        },
    }
}

pub fn apply_lambda<S: ?Sized>(procedure: &Procedure,
                                  args: Vec<Value>,
                                  ctx: &mut LoadedContext<S>) -> AresResult<Value>
where S: State {
    for arg in &args {
        if let &Value::ForeignFn(ForeignFunction { typ: FfType::Ast, .. }) = arg {
            return Err(AresError::AstFunctionPass);
        }
    }

    let mut new_env = try!(procedure.gen_env(args));
    ctx.with_other_env(&mut new_env, |ctx| {
        let mut last = None;
        for body in &*procedure.bodies {
            last = Some(try!(ctx.eval(body)));
        }
        // it's impossible to make a lambda without a body
        Ok(last.unwrap())
    })
}

pub fn apply_function<S: ?Sized>(function: &ForeignFunction<()>,
                                 args: Vec<Value>,
                                 ctx: &mut LoadedContext<S>) -> AresResult<Value>
where S: State {
    for arg in &args {
        if let &Value::ForeignFn(ForeignFunction { typ: FfType::Ast, .. }) = arg {
            return Err(AresError::AstFunctionPass);
        }
    }

    let corrected = try!(function.correct::<S>().or(Err(AresError::InvalidForeignFunctionState)));
    // FIXME pass the whole vec in
    (corrected.function)(&args[..], ctx)
}

pub fn apply<'a, S: ?Sized>(func: &Value,
                            args: Vec<Value>,
                            ctx: &mut LoadedContext<S>) -> AresResult<Value>
where S: State {
    match func.clone() {
        Value::Lambda(procedure, _) => {
            apply_lambda(&procedure, args, ctx)
        }
        Value::ForeignFn(ff) => {
            apply_function(&ff, args, ctx)
        }
        other => Err(AresError::UnexecutableValue(other.clone())),
    }
}

impl ::std::fmt::Debug for StepState {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        match self {
            &StepState::EvalThis(ref v) =>
                formatter.debug_tuple("EvalThis")
                         .field(v)
                         .finish(),
            &StepState::Return =>
                formatter.debug_tuple("Return")
                         .finish(),
            &StepState::Complete(ref v) =>
                formatter.debug_tuple("Complete")
                         .field(v)
                         .finish(),
            &StepState::Lambda { ref func, ref evaluated, .. } =>
                formatter.debug_struct("Lambda")
                         .field("func", func)
                         .field("evaluated", evaluated)
                         .field("unevaluated", &"[..]")
                         .finish(),
            &StepState::PreEvaluatedLambda { .. } =>
                formatter.debug_struct("PreEvaluatedLambda")
                         .field("unevaluated", &"[..]")
                         .finish(),
        }
    }
}
