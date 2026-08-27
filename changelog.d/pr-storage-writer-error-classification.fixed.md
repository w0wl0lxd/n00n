Fixed the session storage writer retrying an oversized transcript record five times per save instead of dropping it immediately, which was flooding the log with warnings.
