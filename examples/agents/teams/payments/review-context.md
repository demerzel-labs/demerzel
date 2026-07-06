# Payments team review context

Payment mutations must be idempotent and audit-logged. Flag money-handling code that
lacks idempotency keys, uses floating-point for currency, or omits transaction
boundaries.
