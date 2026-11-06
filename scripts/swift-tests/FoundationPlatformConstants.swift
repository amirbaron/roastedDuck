// Darwin exports this POSIX time-unit constant; Linux Foundation does not.
// Keep the upstream FoundationTests assertions and conversions unchanged.
#if os(Linux)
let USEC_PER_SEC: UInt64 = 1_000_000
#endif
