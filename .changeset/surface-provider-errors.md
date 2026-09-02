---
"@nomicfoundation/edr": patch
---

Fixed unexpected failures reported from the provider's thread being discarded. Several EDR crates report them through `log`, including an interval mine that produced no block, and those records had no destination in the N-API bindings. `RUST_LOG` controls what is shown, as before.
