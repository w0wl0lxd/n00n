Generate `Idempotency-Key` headers for provider POST requests and reuse them across retries, so transport failures after a request leaves the client are retried safely.
