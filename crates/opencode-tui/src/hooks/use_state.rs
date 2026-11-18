use std::cell::RefCell;
use std::rc::Rc;

pub struct UseState<T> {
    value: Rc<RefCell<T>>,
    changed: Rc<RefCell<bool>>,
}

impl<T: Clone> UseState<T> {
    pub fn new(initial: T) -> Self {
        Self {
            value: Rc::new(RefCell::new(initial)),
            changed: Rc::new(RefCell::new(false)),
        }
    }

    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }

    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
        *self.changed.borrow_mut() = true;
    }

    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        f(&mut self.value.borrow_mut());
        *self.changed.borrow_mut() = true;
    }

    pub fn changed(&self) -> bool {
        *self.changed.borrow()
    }

    pub fn clear_changed(&self) {
        *self.changed.borrow_mut() = false;
    }
}

impl<T: Clone> Clone for UseState<T> {
    fn clone(&self) -> Self {
        Self {
            value: Rc::clone(&self.value),
            changed: Rc::clone(&self.changed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_initializes_value_and_changed_flag() {
        let state = UseState::new(10);

        assert_eq!(state.get(), 10);
        assert!(!state.changed());
    }

    #[test]
    fn set_updates_value_and_marks_changed() {
        let state = UseState::new(1);

        state.set(2);

        assert_eq!(state.get(), 2);
        assert!(state.changed());
    }

    #[test]
    fn update_mutates_value_and_marks_changed() {
        let state = UseState::new(vec![1, 2, 3]);

        state.update(|values| values.push(4));

        assert_eq!(state.get(), vec![1, 2, 3, 4]);
        assert!(state.changed());
    }

    #[test]
    fn clear_changed_resets_flag_but_keeps_value() {
        let state = UseState::new(5);
        state.set(6);

        state.clear_changed();

        assert!(!state.changed());
        assert_eq!(state.get(), 6);
    }

    #[test]
    fn clear_changed_is_idempotent() {
        let state = UseState::new("ready");

        state.clear_changed();
        state.clear_changed();

        assert!(!state.changed());
        assert_eq!(state.get(), "ready");
    }

    #[test]
    fn set_marks_changed_even_when_value_is_unchanged() {
        let state = UseState::new(3);
        state.clear_changed();

        state.set(3);

        assert_eq!(state.get(), 3);
        assert!(state.changed());
    }

    #[test]
    fn update_marks_changed_even_when_closure_is_noop() {
        let state = UseState::new(7);
        state.clear_changed();

        state.update(|_| {});

        assert_eq!(state.get(), 7);
        assert!(state.changed());
    }

    #[test]
    fn get_returns_a_detached_clone() {
        let state = UseState::new(String::from("base"));

        let mut snapshot = state.get();
        snapshot.push('!');

        assert_eq!(state.get(), "base");
        assert!(!state.changed());
    }

    #[test]
    fn clone_shares_the_same_value_and_changed_state() {
        let state = UseState::new(1);
        let alias = state.clone();

        alias.set(2);

        assert_eq!(state.get(), 2);
        assert!(state.changed());
        assert!(alias.changed());

        state.clear_changed();
        assert!(!alias.changed());
    }
}
