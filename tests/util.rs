extern crate ares;

use ::ares::*;

use std::rc::Rc;
use std::cell::RefCell;

#[macro_export]
macro_rules! eval_ok {
    ($prog: expr, $v: expr) => {
        assert_eq!(util::e($prog).unwrap(), $v.into());
    }
}

fn basic_environment() -> Rc<RefCell<Environment>> {
    let mut env = Environment::new();
    stdlib::load_all(&mut env);
    Rc::new(RefCell::new(env))
}

pub fn e(program: &str) -> AresResult<Value> {
    let trees = parse(program).unwrap();
    let mut env = basic_environment();
    let mut last = None;
    for tree in trees {
        last = Some(try!(eval(&tree, &mut env)))
    }
    Ok(last.expect("no program found"))
}
