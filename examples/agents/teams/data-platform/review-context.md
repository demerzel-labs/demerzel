# Data Platform team review context

Schema migrations must be online-safe and reversible. Flag migrations that lock large
tables, drop columns without a deprecation window, or lack a rollback path.
