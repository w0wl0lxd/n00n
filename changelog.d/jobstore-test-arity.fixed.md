Fixed the `kill_job_terminates_long_running_child` test, which still called the
pre-ownership `JobStore` API and broke the build on `main`. Plugin-owned jobs
added an `owner` argument to `start` and `task_id`/`plugin` arguments to `kill`
and `take_receiver`; the restored kill-coverage test was written against the old
signatures, so the two changes merged cleanly as text but did not compile
together.
