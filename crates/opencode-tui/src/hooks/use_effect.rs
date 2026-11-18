use std::cell::RefCell;
use std::rc::Rc;

type CleanupFn = Box<dyn FnMut()>;

pub struct UseEffect {
    cleanup: Rc<RefCell<Option<CleanupFn>>>,
}

impl UseEffect {
    pub fn new() -> Self {
        Self {
            cleanup: Rc::new(RefCell::new(None)),
        }
    }

    pub fn run<F>(&self, effect: F)
    where
        F: FnOnce() -> Option<Box<dyn FnMut()>>,
    {
        if let Some(mut cleanup) = self.cleanup.borrow_mut().take() {
            cleanup();
        }

        if let Some(new_cleanup) = effect() {
            *self.cleanup.borrow_mut() = Some(new_cleanup);
        }
    }
}

impl Default for UseEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for UseEffect {
    fn drop(&mut self) {
        if let Some(mut cleanup) = self.cleanup.borrow_mut().take() {
            cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_default_run_effects_without_pending_cleanup() {
        for effect in [UseEffect::new(), UseEffect::default()] {
            let calls = Rc::new(RefCell::new(0));
            let counter = Rc::clone(&calls);

            effect.run(move || {
                *counter.borrow_mut() += 1;
                None
            });

            assert_eq!(*calls.borrow(), 1);
        }
    }

    #[test]
    fn run_invokes_the_effect_each_time() {
        let effect = UseEffect::new();
        let calls = Rc::new(RefCell::new(0));
        let counter = Rc::clone(&calls);

        effect.run(|| {
            *counter.borrow_mut() += 1;
            None
        });
        effect.run(|| {
            *counter.borrow_mut() += 1;
            None
        });

        assert_eq!(*calls.borrow(), 2);
    }

    #[test]
    fn run_calls_previous_cleanup_and_stores_new_one() {
        let effect = UseEffect::new();
        let first_calls = Rc::new(RefCell::new(0));
        let second_calls = Rc::new(RefCell::new(0));

        let first = Rc::clone(&first_calls);
        effect.run(move || Some(Box::new(move || *first.borrow_mut() += 1)));

        assert_eq!(*first_calls.borrow(), 0);

        let second = Rc::clone(&second_calls);
        effect.run(move || Some(Box::new(move || *second.borrow_mut() += 1)));

        assert_eq!(*first_calls.borrow(), 1);
        assert_eq!(*second_calls.borrow(), 0);

        drop(effect);

        assert_eq!(*first_calls.borrow(), 1);
        assert_eq!(*second_calls.borrow(), 1);
    }

    #[test]
    fn drop_runs_the_pending_cleanup() {
        let calls = Rc::new(RefCell::new(0));

        {
            let effect = UseEffect::new();
            let counter = Rc::clone(&calls);
            effect.run(move || Some(Box::new(move || *counter.borrow_mut() += 1)));
            assert_eq!(*calls.borrow(), 0);
        }

        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn run_without_new_cleanup_discards_the_previous_one() {
        let effect = UseEffect::new();
        let calls = Rc::new(RefCell::new(0));

        let counter = Rc::clone(&calls);
        effect.run(move || Some(Box::new(move || *counter.borrow_mut() += 1)));

        // The second run consumes and invokes the stored cleanup, then stores nothing.
        effect.run(|| None);
        assert_eq!(*calls.borrow(), 1);

        drop(effect);
        assert_eq!(*calls.borrow(), 1);
    }
}
