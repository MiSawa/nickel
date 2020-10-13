//! Evaluation of a Nickel term.
//!
//! The implementation of the Nickel abstract machine which evaluates a term. Note that this
//! machine is not currently formalized somewhere and is just a convenient name to designate the
//! current implementation.
//!
//! # The Nickel Abstract Machine
//! The abstract machine is a stack machine composed of the following elements:
//! - The term being currently evaluated
//! - The main stack, storing arguments, thunks and pending computations
//! - A pair of [environments](type.Environment.html), mapping identifiers to [closures](type.Closure.html):
//!     * The global environment contains builtin functions accessible from anywhere, and alive
//!     during the whole evaluation
//!     * The local environment contains the variables in scope of the current term and is subject
//!     to garbage collection (currently reference counting based)
//! - A [callstack](type.CallStack.html), mainly for error reporting purpose
//!
//! Depending on the shape of the current term, the following actions are preformed:
//!
//! ## Core calculus
//! - **Var(id)**: the term bound to `id` in the environment is fetched, and an update thunk is
//! pushed on the stack to indicate that once this term has been evaluated, the content of the
//! variable must be updated
//! - **App(func, arg)**: a closure containing the argument and the current environment is pushed
//! on the stack, and the applied term `func` is evaluated
//! - **Let(id, term, body)**: `term` is bound to `id` in the environment, and the machine proceeds with the evaluation of the body
//! - **Fun(id, body)**: Try to pop an argument from the stack. If there is some, we bound it to
//! `id` in the environment, and proceed with the body of the function. Otherwise, we are done: the
//! end result is an unapplied function
//! - **Thunk on stack**: If the evaluation of the current term is done, and there is one (or
//! several) thunk on the stack, this means we have to perform an update. Consecutive thunks are
//! popped from the stack and are updated to point to the current evaluated term.
//! - **Import**: Import must have been resolved before the evaluation starts. An unresolved import
//! causes an [`InternalError`](../error/enum.EvalError.html#variant.InternalError). A resolved
//! import, identified by a `FileId`, is retrieved from the import resolver and evaluation proceeds.
//!
//! ## Contracts
//!
//! - **`Assume(type, label, term)`** (or `Promise(type, label, term)`): replace the current term
//! with the contract corresponding to `types`, applied to label and term (`contract label term`).
//!
//! ## Operators
//!
//! Operators are strict by definition. To evaluate say `exp1 + exp2`, the following steps
//! have to be performed:
//! - `exp1` needs to be evaluated. The result must be saved somewhere, together with the resulting
//! environment
//! - `exp2`: same thing for `exp2`
//! - Finally, the implementation of `+` can proceed with the computation
//!
//! We detail the case of binary operators, as the case of unary ones is similar and simpler.
//!
//! - **Op(op, first, second)**: push an `OpFirst` element on the stack, which saves the operator
//! `op`, the second argument `second` and the current environment, and proceed with the evaluation
//! of `first`
//! - **OpFirst on stack**: if the evaluation of the current term is done and there is an `OpFirst`
//! marker on the stack, then:
//!     1. Extract the saved operator, the second argument and the environment `env2` from the marker
//!     2. Push an `OpSecond` marker, saving the operator and the evaluated form of the first
//!        argument with its environment
//!     3. Proceed with the evaluation of the second argument in environment `env2`
//! - **OpSecond on stack**: once the second term is evaluated, we can get back the operator and
//! the first term evaluated, and forward all both arguments evaluated and their respective
//! environment to the specific implementation of the operator (located in
//! [operation](../operation/index.html), or in [merge](../merge/index.html) for `merge`).
//!
//! ## Enriched values
//!
//! The evaluation of enriched values is controlled by the parameter `enriched_strict`. If it is
//! set to true (which is usually the case), the machine tries to extract a simple value from it:
//!  - **Contract**: raise an error. This usually means that an access to a field was attempted,
//!  and that this field had a contract to satisfy, but it was never defined.
//!  - **Default(value)**: an access to a field which has a default value. Proceed with the
//!  evaluation of this value
//!  - **ContractDefault(type, label, value)**: same as above, but the field also has an attached
//!  contract.  Proceed with the evaluation of `Assume(type, label, value)` to ensure that the
//!  default value satisfies this contract.
//!
//!  If `enriched_strict` is set to false, as it is when evaluating `merge`, the machine does not
//!  evaluate enriched values further, and consider the term evaluated.
//!
//! # Garbage collection
//!
//! Currently the machine relies on Rust's reference counting to manage memory. Precisely, the
//! environment stores `Rc<RefCell<Closure>>` objects, which are reference-counted pointers to a
//! mutable memory cell. This means that we do not deep copy everything everywhere, but this is
//! probably suboptimal for a functional language and is unable to collect cyclic data, which may
//! appear inside recursive records in the future. An adapted garbage collector is probably
//! something to consider at some point.
use crate::error::EvalError;
use crate::identifier::Ident;
use crate::operation::{continuate_operation, OperationCont};
use crate::position::RawSpan;
use crate::program::ImportResolver;
use crate::stack::Stack;
use crate::term::{RichTerm, StrChunk, Term, UnaryOp};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::{Rc, Weak};

/// An environment, which is a mapping from identifiers to closures.
pub type Environment = HashMap<Ident, (Rc<RefCell<Closure>>, IdentKind)>;

/// A call stack, saving the history of function calls.
///
/// In a lazy language as Nickel, there are no well delimited stack frames due to how function
/// application is evaluated. This can make things hard to debug for the user, hence additional
/// information about the history of function calls is stored in the call stack, for error
/// reporting and debugging purposes.
pub type CallStack = Vec<StackElem>;

/// A call stack element.
#[derive(Debug, PartialEq, Clone)]
pub enum StackElem {
    App(Option<RawSpan>),
    Var(IdentKind, Ident, Option<RawSpan>),
}

/// Kind of an identifier.
#[derive(Debug, PartialEq, Clone)]
pub enum IdentKind {
    Let(),
    Lam(),
    Record(),
}

/// A closure, a term together with an environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    pub body: RichTerm,
    pub env: Environment,
}

impl Closure {
    pub fn atomic_closure(body: RichTerm) -> Closure {
        Closure {
            body,
            env: HashMap::new(),
        }
    }
}

/// Determine if a thunk is worth being put on the stack for future update.
///
/// Typically, WHNFs and enriched values will not be evaluated to a simpler expression and are not
/// worth updating.
fn should_update(t: &Term) -> bool {
    !t.is_whnf() && !t.is_enriched()
}

/// The main loop of evaluation.
///
/// Implement the evaluation of the core language, which includes application, thunk update,
/// evaluation of the arguments of operations, and a few others. The specific implementations of
/// primitive operations is delegated to the modules [operation](../operation/index.html) and
/// [merge](../merge/index.html).
///
/// # Arguments
///
/// - `t0`: the term to evaluate
/// - `global_env`: the global environment containing the builtin functions of the language. Accessible from anywhere in the
/// program.
/// - `resolver`: the interface to fetch imports.
pub fn eval<R>(t0: RichTerm, global_env: Environment, resolver: &mut R) -> Result<Term, EvalError>
where
    R: ImportResolver,
{
    let mut clos = Closure::atomic_closure(t0);
    let mut call_stack = CallStack::new();
    let mut stack = Stack::new();
    let mut enriched_strict = true;

    loop {
        println!("EVAL LOOP ---------");
        println!("Evaluating: {}", clos);

        let Closure {
            body: RichTerm {
                term: boxed_term,
                pos,
            },
            mut env,
        } = clos;
        let term = *boxed_term;

        clos = match term {
            Term::Var(x) => {
                let (thunk, id_kind) = env
                    .remove(&x)
                    .or_else(|| {
                        global_env
                            .get(&x)
                            .map(|(rc, id_kind)| (rc.clone(), id_kind.clone()))
                    })
                    .ok_or(EvalError::UnboundIdentifier(x.clone(), pos.clone()))?;
                std::mem::drop(env); // thunk may be a 1RC pointer
                if should_update(&thunk.borrow().body.term) {
                    stack.push_thunk(Rc::downgrade(&thunk));
                }
                call_stack.push(StackElem::Var(id_kind, x, pos));
                match Rc::try_unwrap(thunk) {
                    Ok(c) => {
                        // thunk was the only strong ref to the closure
                        c.into_inner()
                    }
                    Err(rc) => {
                        // We need to clone it, there are other strong refs
                        rc.borrow().clone()
                    }
                }
            }
            Term::App(t1, t2) => {
                stack.push_arg(
                    Closure {
                        body: t2,
                        env: env.clone(),
                    },
                    pos,
                );
                Closure { body: t1, env }
            }
            Term::Let(x, s, t) => {
                let thunk = Rc::new(RefCell::new(Closure {
                    body: s,
                    env: env.clone(),
                }));
                env.insert(x, (Rc::clone(&thunk), IdentKind::Let()));
                Closure { body: t, env }
            }
            Term::Op1(op, t) => {
                let op = op.map(|t| Closure {
                    body: t,
                    env: env.clone(),
                });

                stack.push_op_cont(OperationCont::Op1(op, t.pos.clone()), call_stack.len(), pos);
                Closure { body: t, env }
            }
            Term::Op2(op, fst, snd) => {
                let op = op.map(|t| Closure {
                    body: t,
                    env: env.clone(),
                });

                let prev_strict = enriched_strict;
                enriched_strict = op.is_strict();
                stack.push_op_cont(
                    OperationCont::Op2First(
                        op,
                        Closure {
                            body: snd,
                            env: env.clone(),
                        },
                        fst.pos.clone(),
                        prev_strict,
                    ),
                    call_stack.len(),
                    pos,
                );
                Closure { body: fst, env }
            }
            Term::StrChunks(mut chunks) => match chunks.pop() {
                None => Closure {
                    body: Term::Str(String::new()).into(),
                    env: HashMap::new(),
                },
                Some(StrChunk::Literal(s)) => Closure {
                    body: Term::Op1(
                        UnaryOp::ChunksConcat(String::new(), chunks),
                        Term::Str(s).into(),
                    )
                    .into(),
                    env: HashMap::new(),
                },
                Some(StrChunk::Expr(e)) => Closure {
                    body: RichTerm {
                        term: Box::new(Term::Op1(UnaryOp::ChunksConcat(String::new(), chunks), e)),
                        pos,
                    },
                    env,
                },
            },
            Term::Promise(ty, l, t) | Term::Assume(ty, l, t) => {
                stack.push_arg(
                    Closure {
                        body: t,
                        env: env.clone(),
                    },
                    None,
                );
                stack.push_arg(Closure::atomic_closure(RichTerm::new(Term::Lbl(l))), None);
                Closure {
                    body: ty.contract(),
                    env,
                }
            }
            Term::RecRecord(ts) => {
                // Thanks to the share normal form transformation, the content is either a constant or a
                // variable.
                let rec_env =
                    ts.iter()
                        .try_fold(HashMap::new(), |mut rec_env, (id, rt)| match rt.as_ref() {
                            &Term::Var(ref var_id) => {
                                let (thunk, id_kind) = env.get(var_id).ok_or(
                                    EvalError::UnboundIdentifier(var_id.clone(), rt.pos.clone()),
                                )?;
                                rec_env.insert(id.clone(), (thunk.clone(), id_kind.clone()));
                                Ok(rec_env)
                            }
                            _ => {
                                // If we are in this branch, the term must be a constant after the
                                // share normal form transformation, hence it should not need an
                                // environment, which is it is dropped.
                                let closure = Closure {
                                    body: rt.clone(),
                                    env: HashMap::new(),
                                };
                                rec_env.insert(
                                    id.clone(),
                                    (Rc::new(RefCell::new(closure)), IdentKind::Let()),
                                );
                                Ok(rec_env)
                            }
                        })?;

                let new_ts = ts.into_iter().map(|(id, rt)| {
                    let RichTerm { term, pos } = rt;
                    match *term {
                        Term::Var(var_id) => {
                            // We already checked for unbound identifier in the previous fold, so this
                            // get should always succeed.
                            let (thunk, _) = env.get(&var_id).unwrap();
                            thunk.borrow_mut().env.extend(rec_env.clone());
                            (
                                id,
                                RichTerm {
                                    term: Box::new(Term::Var(var_id)),
                                    pos,
                                },
                            )
                        }
                        _ => (id, RichTerm { term, pos }),
                    }
                });
                Closure {
                    body: RichTerm {
                        term: Box::new(Term::Record(new_ts.collect())),
                        pos,
                    },
                    env,
                }
            }
            // Unwrapping of enriched terms
            Term::Contract(_, _) if enriched_strict => {
                return Err(EvalError::Other(
                    String::from(
                        "Expected a simple term, got a Contract. Contracts cannot be evaluated",
                    ),
                    pos,
                ));
            }
            enriched @ Term::DefaultValue(_) | enriched @ Term::Docstring(_, _)
                if enriched_strict =>
            {
                /* Since we are forcing an enriched value, we are morally breaking subject
                 * reduction (i.e., the type of the current term changes from `enriched something`
                 * to just `something`). Updating a thunk after having performed this forcing may
                 * alter the semantics of the program in an unexpected way (see issue
                 * https://github.com/tweag/nickel/issues/123): we update potential thunks now so
                 * that their content remains an enriched value.
                 */
                let update_closure = Closure {
                    body: RichTerm {
                        term: Box::new(enriched),
                        pos,
                    },
                    env,
                };
                update_thunks(&mut stack, &update_closure);

                let Closure {
                    body:
                        RichTerm {
                            term: enriched_box,
                            pos: _,
                        },
                    env,
                } = update_closure;
                let t = match *enriched_box {
                    Term::DefaultValue(t) | Term::Docstring(_, t) => t,
                    _ => panic!("eval::eval(): previous match enforced that a term is a default or a docstring, but matched something else")
                };
                Closure { body: t, env }
            }
            Term::ContractWithDefault(ty, label, t) if enriched_strict => Closure {
                body: Term::Assume(ty, label, t).into(),
                env,
            },
            Term::ResolvedImport(id) => {
                if let Some(t) = resolver.get(id) {
                    Closure::atomic_closure(t)
                } else {
                    return Err(EvalError::InternalError(
                        format!("Resolved import not found ({:?})", id),
                        pos,
                    ));
                }
            }
            Term::Import(path) => {
                return Err(EvalError::InternalError(
                    format!("Unresolved import ({})", path),
                    pos,
                ))
            }
            // Continuation of operations and thunk update
            _ if stack.is_top_thunk() || stack.is_top_cont() => {
                clos = Closure {
                    body: RichTerm {
                        term: Box::new(term),
                        pos,
                    },
                    env,
                };
                if stack.is_top_thunk() {
                    update_thunks(&mut stack, &clos);
                    clos
                } else {
                    let cont_result = continuate_operation(
                        clos,
                        &mut stack,
                        &mut call_stack,
                        &mut enriched_strict,
                    );

                    if let Err(EvalError::BlameError(l, _)) = cont_result {
                        return Err(EvalError::BlameError(l, Some(call_stack)));
                    }
                    cont_result?
                }
            }
            // Function call
            Term::Fun(x, t) => {
                if 0 < stack.count_args() {
                    let (arg, pos_app) = stack.pop_arg().expect("Condition already checked.");
                    call_stack.push(StackElem::App(pos_app));
                    let thunk = Rc::new(RefCell::new(arg));
                    env.insert(x, (thunk, IdentKind::Lam()));
                    Closure { body: t, env }
                } else {
                    return Ok(Term::Fun(x, t));
                }
            }
            // Otherwise, this is either an ill-formed application, or we are done
            t => {
                if 0 < stack.count_args() {
                    let (arg, pos_app) = stack.pop_arg().expect("Condition already checked.");
                    return Err(EvalError::NotAFunc(
                        RichTerm {
                            term: Box::new(t),
                            pos,
                        },
                        arg.body,
                        pos_app,
                    ));
                } else {
                    return Ok(t);
                }
            }
        }
    }
}

/// Pop and update all the thunks on the top of the stack with the given closure.
fn update_thunks(stack: &mut Stack, closure: &Closure) {
    while let Some(thunk) = stack.pop_thunk() {
        if let Some(safe_thunk) = Weak::upgrade(&thunk) {
            *safe_thunk.borrow_mut() = closure.clone();
        }
    }
}

impl fmt::Display for Closure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let Closure { body, env } = self;

        write!(f, "<closure:\n")?;
        write!(f, "{}\n", pprint::fmt_indent(body, false))?;
        write!(f, "| in env:\n")?;
        write!(
            f,
            "{}",
            pprint::fmt_indent(&pprint::EnvFormatter::from_env(env), false)
        )?;
        write!(f, ">")
    }
}

mod pprint {
    use super::*;
    use std::fmt::Write;

    pub struct EnvFormatter<'a>(&'a Environment);

    impl<'a> EnvFormatter<'a> {
        pub fn from_env(env: &'a Environment) -> Self {
            EnvFormatter(env)
        }
    }

    pub fn fmt_indent<D>(val: &D, skip_fst: bool) -> String
    where
        D: fmt::Display,
    {
        let mut buffer = String::new();
        write!(&mut buffer, "{}", val).unwrap();
        let lines: Vec<String> = buffer
            .lines()
            .enumerate()
            .map(|(u, s)| {
                if u == 0 && skip_fst {
                    String::from(s)
                } else {
                    format!("  {}", s)
                }
            })
            .collect();
        lines.join("\n")
    }

    impl<'a> fmt::Display for EnvFormatter<'a> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "<env:{{")?;

            if self.0.len() > 1 {
                write!(f, "\n")?;
            }

            self.0.iter().try_for_each(|(id, (rc, _))| {
                write!(f, "{} = {};", id, fmt_indent(&rc.borrow(), true))?;

                if self.0.len() > 1 {
                    write!(f, "\n")?;
                }

                Ok(())
            })?;

            write!(f, "}}>")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ImportError;
    use crate::label::Label;
    use crate::program::resolvers::{DummyResolver, SimpleResolver};
    use crate::term::StrChunk;
    use crate::term::{BinaryOp, UnaryOp};
    use crate::transformations::transform;

    /// Evaluate a term without import support.
    fn eval_no_import(t: RichTerm) -> Result<Term, EvalError> {
        eval(t, HashMap::new(), &mut DummyResolver {})
    }

    #[test]
    fn identity_over_values() {
        let num = Term::Num(45.3);
        assert_eq!(Ok(num.clone()), eval_no_import(num.into()));

        let boolean = Term::Bool(true);
        assert_eq!(Ok(boolean.clone()), eval_no_import(boolean.into()));

        let lambda = Term::Fun(
            Ident("x".to_string()),
            RichTerm::app(RichTerm::var("x".into()), RichTerm::var("x".into())),
        );
        assert_eq!(Ok(lambda.clone()), eval_no_import(lambda.into()));
    }

    #[test]
    fn blame_panics() {
        let label = Label::dummy();
        if let Err(EvalError::BlameError(l, _)) =
            eval_no_import(Term::Op1(UnaryOp::Blame(), Term::Lbl(label.clone()).into()).into())
        {
            assert_eq!(l, label);
        } else {
            panic!("This evaluation should've returned a BlameError!");
        }
    }

    #[test]
    #[should_panic]
    fn lone_var_panics() {
        eval_no_import(RichTerm::var("unbound".into())).unwrap();
    }

    #[test]
    fn only_fun_are_applicable() {
        eval_no_import(RichTerm::app(Term::Bool(true).into(), Term::Num(45.).into()).into())
            .unwrap_err();
    }

    #[test]
    fn simple_app() {
        let t = RichTerm::app(
            Term::Fun(Ident("x".to_string()), RichTerm::var("x".into())).into(),
            Term::Num(5.0).into(),
        );

        assert_eq!(Ok(Term::Num(5.0)), eval_no_import(t));
    }

    #[test]
    fn simple_let() {
        let t = RichTerm::let_in("x", Term::Num(5.0).into(), RichTerm::var("x".into()));

        assert_eq!(Ok(Term::Num(5.0)), eval_no_import(t));
    }

    #[test]
    fn simple_ite() {
        let t = RichTerm::ite(
            Term::Bool(true).into(),
            Term::Num(5.0).into(),
            Term::Bool(false).into(),
        );

        assert_eq!(Ok(Term::Num(5.0)), eval_no_import(t));
    }

    #[test]
    fn simple_plus() {
        let t = RichTerm::plus(Term::Num(5.0).into(), Term::Num(7.5).into());

        assert_eq!(Ok(Term::Num(12.5)), eval_no_import(t));
    }

    #[test]
    fn simple_is_zero() {
        let t = Term::Op1(UnaryOp::IsZero(), Term::Num(7.0).into()).into();

        assert_eq!(Ok(Term::Bool(false)), eval_no_import(t));
    }

    #[test]
    fn asking_for_various_types() {
        let num = Term::Op1(UnaryOp::IsNum(), Term::Num(45.3).into()).into();
        assert_eq!(Ok(Term::Bool(true)), eval_no_import(num));

        let boolean = Term::Op1(UnaryOp::IsBool(), Term::Bool(true).into()).into();
        assert_eq!(Ok(Term::Bool(true)), eval_no_import(boolean));

        let lambda = Term::Op1(
            UnaryOp::IsFun(),
            Term::Fun(
                Ident("x".to_string()),
                RichTerm::app(RichTerm::var("x".into()), RichTerm::var("x".into())),
            )
            .into(),
        )
        .into();
        assert_eq!(Ok(Term::Bool(true)), eval_no_import(lambda));
    }

    #[test]
    fn enriched_terms_unwrapping() {
        let t = Term::DefaultValue(
            Term::DefaultValue(Term::Docstring("a".to_string(), Term::Bool(false).into()).into())
                .into(),
        )
        .into();
        assert_eq!(Ok(Term::Bool(false)), eval_no_import(t));
    }

    #[test]
    fn merge_enriched_default() {
        let t = Term::Op2(
            BinaryOp::Merge(),
            Term::Num(1.0).into(),
            Term::DefaultValue(Term::Num(2.0).into()).into(),
        )
        .into();
        assert_eq!(Ok(Term::Num(1.0)), eval_no_import(t));
    }

    #[test]
    fn merge_incompatible_defaults() {
        let t = Term::Op2(
            BinaryOp::Merge(),
            Term::DefaultValue(Term::Num(1.0).into()).into(),
            Term::DefaultValue(Term::Num(2.0).into()).into(),
        )
        .into();

        eval_no_import(t).unwrap_err();
    }

    #[test]
    fn imports() {
        let mut resolver = SimpleResolver::new();
        resolver.add_source(String::from("two"), String::from("1 + 1"));
        resolver.add_source(String::from("lib"), String::from("{ f = true }"));
        resolver.add_source(String::from("bad"), String::from("^$*/.23ab 0°@"));
        resolver.add_source(
            String::from("nested"),
            String::from("let x = import \"two\" in x + 1"),
        );
        resolver.add_source(
            String::from("cycle"),
            String::from("let x = import \"cycle_b\" in {a = 1; b = x.a}"),
        );
        resolver.add_source(
            String::from("cycle_b"),
            String::from("let x = import \"cycle\" in {a = x.a}"),
        );

        fn mk_import<R>(
            var: &str,
            import: &str,
            body: RichTerm,
            resolver: &mut R,
        ) -> Result<RichTerm, ImportError>
        where
            R: ImportResolver,
        {
            transform(
                RichTerm::let_in(var, Term::Import(String::from(import)).into(), body),
                resolver,
            )
        };

        // let x = import "does_not_exist" in x
        match mk_import(
            "x",
            "does_not_exist",
            RichTerm::var(String::from("x")),
            &mut resolver,
        )
        .unwrap_err()
        {
            ImportError::IOError(_, _, _) => (),
            _ => assert!(false),
        };

        // let x = import "bad" in x
        match mk_import("x", "bad", RichTerm::var(String::from("x")), &mut resolver).unwrap_err() {
            ImportError::ParseError(_, _) => (),
            _ => assert!(false),
        };

        // let x = import "two" in x
        assert_eq!(
            eval(
                mk_import("x", "two", RichTerm::var(String::from("x")), &mut resolver).unwrap(),
                HashMap::new(),
                &mut resolver
            )
            .unwrap(),
            Term::Num(2.0)
        );

        // let x = import "nested" in x
        assert_eq!(
            eval(
                mk_import(
                    "x",
                    "nested",
                    RichTerm::var(String::from("x")),
                    &mut resolver
                )
                .unwrap(),
                HashMap::new(),
                &mut resolver
            )
            .unwrap(),
            Term::Num(3.0)
        );

        // let x = import "lib" in x.f
        assert_eq!(
            eval(
                mk_import(
                    "x",
                    "lib",
                    Term::Op1(
                        UnaryOp::StaticAccess(Ident(String::from("f"))),
                        RichTerm::var(String::from("x"))
                    )
                    .into(),
                    &mut resolver,
                )
                .unwrap(),
                HashMap::new(),
                &mut resolver
            )
            .unwrap(),
            Term::Bool(true)
        );

        // let x = import "cycle" in x.b
        assert_eq!(
            eval(
                mk_import(
                    "x",
                    "cycle",
                    Term::Op1(
                        UnaryOp::StaticAccess(Ident(String::from("b"))),
                        RichTerm::var(String::from("x"))
                    )
                    .into(),
                    &mut resolver,
                )
                .unwrap(),
                HashMap::new(),
                &mut resolver
            )
            .unwrap(),
            Term::Num(1.0)
        );
    }

    #[test]
    fn interpolation_simple() {
        let mut chunks = vec![
            StrChunk::Literal(String::from("Hello")),
            StrChunk::Expr(
                Term::Op2(
                    BinaryOp::PlusStr(),
                    Term::Str(String::from(", ")).into(),
                    Term::Str(String::from("World!")).into(),
                )
                .into(),
            ),
            StrChunk::Literal(String::from(" How")),
            StrChunk::Expr(RichTerm::ite(
                Term::Bool(true).into(),
                Term::Str(String::from(" are")).into(),
                Term::Str(String::from(" is")).into(),
            )),
            StrChunk::Literal(String::from(" you?")),
        ];
        chunks.reverse();

        let t: RichTerm = Term::StrChunks(chunks).into();
        assert_eq!(
            eval_no_import(t),
            Ok(Term::Str(String::from("Hello, World! How are you?")))
        );
    }

    #[test]
    fn interpolation_nested() {
        let mut inner_chunks = vec![
            StrChunk::Literal(String::from(" How")),
            StrChunk::Expr(
                Term::Op2(
                    BinaryOp::PlusStr(),
                    Term::Str(String::from(" ar")).into(),
                    Term::Str(String::from("e")).into(),
                )
                .into(),
            ),
            StrChunk::Expr(RichTerm::ite(
                Term::Bool(true).into(),
                Term::Str(String::from(" you")).into(),
                Term::Str(String::from(" me")).into(),
            )),
        ];
        inner_chunks.reverse();

        let mut chunks = vec![
            StrChunk::Literal(String::from("Hello, World!")),
            StrChunk::Expr(Term::StrChunks(inner_chunks).into()),
            StrChunk::Literal(String::from("?")),
        ];
        chunks.reverse();

        let t: RichTerm = Term::StrChunks(chunks).into();
        assert_eq!(
            eval_no_import(t),
            Ok(Term::Str(String::from("Hello, World! How are you?")))
        );
    }

    #[test]
    fn global_env() {
        let mut global_env = HashMap::new();
        let mut resolver = DummyResolver {};
        let thunk = Rc::new(RefCell::new(Closure {
            body: Term::Num(1.0).into(),
            env: HashMap::new(),
        }));
        global_env.insert(
            Ident(String::from("g")),
            (Rc::clone(&thunk), IdentKind::Let()),
        );

        let t = RichTerm::let_in(
            "x",
            Term::Num(2.0).into(),
            Term::Var(Ident(String::from("x"))).into(),
        );
        assert_eq!(
            eval(t, global_env.clone(), &mut resolver),
            Ok(Term::Num(2.0))
        );

        let t = RichTerm::let_in(
            "x",
            Term::Num(2.0).into(),
            Term::Var(Ident(String::from("g"))).into(),
        );
        assert_eq!(
            eval(t, global_env.clone(), &mut resolver),
            Ok(Term::Num(1.0))
        );

        // Shadowing of global environment
        let t = RichTerm::let_in(
            "g",
            Term::Num(2.0).into(),
            Term::Var(Ident(String::from("g"))).into(),
        );
        assert_eq!(
            eval(t, global_env.clone(), &mut resolver),
            Ok(Term::Num(2.0))
        );
    }
}
