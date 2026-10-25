use std::panic::resume_unwind;
use std::thread;

use anyhow::Context as _;
use anyhow::Result;

pub(crate) fn try_pair<T: Send, U: Send>(
    first: impl FnOnce() -> Result<T> + Send,
    second: impl FnOnce() -> Result<U> + Send,
) -> Result<(T, U)> {
    thread::scope(|scope| {
        let first = thread::Builder::new().spawn_scoped(scope, first);
        let second = thread::Builder::new().spawn_scoped(scope, second);
        let first = match first {
            Ok(worker) => worker.join(),
            Err(error) => Ok(Err(error).context("Cannot start first parallel worker")),
        };
        let second = match second {
            Ok(worker) => worker.join(),
            Err(error) => Ok(Err(error).context("Cannot start second parallel worker")),
        };
        match (first, second) {
            (Err(payload), _) | (_, Err(payload)) => resume_unwind(payload),
            (Ok(first), Ok(second)) => Ok((first?, second?)),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::panic::catch_unwind;
    use std::panic::resume_unwind;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use anyhow::Result;
    use anyhow::anyhow;
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::try_pair;

    #[test]
    fn test_scoped_values() {
        const FIRST: &str = "first";
        let first = String::from(FIRST);
        let second = [1, 2, 3];

        let system_under_test = try_pair(|| Ok(first.as_str()), || Ok(second.as_slice()));

        let values = assert_ok!(system_under_test, "Both scoped workers must complete with their borrowed values");
        assert_eq!(values, (FIRST, second.as_slice()));
    }

    #[rstest]
    #[case::first_error(true, false)]
    #[case::second_error(false, true)]
    #[case::both_errors(true, true)]
    fn test_operation_errors(#[case] first_fails: bool, #[case] second_fails: bool) {
        const MESSAGE_ERROR_FIRST: &str = "First operation failed";
        const MESSAGE_ERROR_SECOND: &str = "Second operation failed";
        let first_finished = AtomicBool::new(false);
        let second_finished = AtomicBool::new(false);

        let system_under_test = try_pair(
            || {
                first_finished.store(true, Ordering::SeqCst);
                if first_fails { Err(anyhow!(MESSAGE_ERROR_FIRST)) } else { Ok(()) }
            },
            || {
                second_finished.store(true, Ordering::SeqCst);
                if second_fails { Err(anyhow!(MESSAGE_ERROR_SECOND)) } else { Ok(()) }
            },
        );

        assert!(first_finished.load(Ordering::SeqCst), "First worker was not collected");
        assert!(second_finished.load(Ordering::SeqCst), "Second worker was not collected");
        assert_eq!(
            system_under_test.err().map(|error| error.to_string()).as_deref(),
            Some(if first_fails { MESSAGE_ERROR_FIRST } else { MESSAGE_ERROR_SECOND })
        );
    }

    #[rstest]
    #[case::first_panics(true)]
    #[case::second_panics(false)]
    fn test_panic_precedes_operation_error(#[case] first_panics: bool) {
        const MESSAGE_ERROR_OPERATION: &str = "The other operation failed";
        let payload = Arc::new(());
        let operation_finished = AtomicBool::new(false);

        let system_under_test = catch_unwind(|| {
            try_pair(
                || -> Result<()> {
                    if first_panics {
                        resume_unwind(Box::new(Arc::clone(&payload)));
                    }
                    operation_finished.store(true, Ordering::SeqCst);
                    Err(anyhow!(MESSAGE_ERROR_OPERATION))
                },
                || -> Result<()> {
                    if !first_panics {
                        resume_unwind(Box::new(Arc::clone(&payload)));
                    }
                    operation_finished.store(true, Ordering::SeqCst);
                    Err(anyhow!(MESSAGE_ERROR_OPERATION))
                },
            )
        });

        assert!(operation_finished.load(Ordering::SeqCst), "The other worker was not collected");
        assert!(
            system_under_test.is_err_and(|panic| {
                panic.downcast_ref::<Arc<()>>().is_some_and(|original| Arc::ptr_eq(original, &payload))
            }),
            "The worker's original panic payload was not preserved"
        );
    }
}
