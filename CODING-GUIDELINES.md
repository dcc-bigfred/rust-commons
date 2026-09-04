# Rust Engineering Best Practices

> A complete project standard for writing correct, explicit, allocation-aware, production-grade Rust.
>
> This document is intended to replace the previous `rust-best-practices.md` Gist in full. It integrates ownership, API design, error handling, testing, linting, performance, `debug_assert!()` usage, and heap-allocation discipline into one coherent set of rules.

## Status and terminology

This is a normative engineering guide, not an introductory Rust tutorial.

The words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** describe requirement strength:

- **MUST / MUST NOT**: required unless an approved architecture decision explicitly documents an exception.
- **SHOULD / SHOULD NOT**: the default; deviations require a concrete reason in code review.
- **MAY**: optional and context-dependent.

Correctness, safety, determinism, and maintainability take priority over cleverness. Performance work must be driven by measurements, but allocation behavior and boundedness should be designed into APIs before profiling because they are architectural properties, not merely local optimizations.

---

## 1. Core engineering principles

### 1.1 Make invalid states difficult or impossible to represent

Use the type system to encode domain meaning and legal states:

- newtypes for identifiers, offsets, lengths, units, scores, and protocol values;
- enums instead of loosely related booleans or sentinel integers;
- `Option<T>` for optional values rather than magic values;
- `Result<T, E>` for recoverable failures;
- type-state when legal operations depend on an object's lifecycle state;
- fixed-width integer types at serialization and FFI boundaries;
- private fields unless direct representation access is intentionally part of the contract.

```rust
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionStatus {
    Exact,
    BackedOff,
    Unsupported,
}
```

Do not use a raw `u32` for every identifier merely because the underlying representation is the same. Distinct types prevent accidental interchange and make signatures self-documenting.

### 1.2 Prefer explicit control flow over hidden behavior

Important behavior should be visible in types and function signatures:

- allocation behavior;
- capacity limits;
- error behavior;
- ownership transfer;
- mutation;
- blocking versus asynchronous behavior;
- determinism requirements;
- panic conditions;
- thread-safety expectations.

Avoid APIs whose correctness depends on undocumented global state, implicit initialization, hidden retries, lazy allocation, or environment-dependent defaults.

### 1.3 Keep runtime work bounded

Runtime and hot-path operations MUST have explicit bounds on:

- iterations;
- candidate counts;
- recursion depth;
- queue length;
- output size;
- temporary storage;
- retry count;
- concurrency;
- bytes read or written where practical.

When a limit is reached, return a typed error or explicit status. Do not silently allocate more memory, recurse without a bound, grow an unbounded queue, or switch to an unbounded fallback algorithm.

### 1.4 Separate rich construction from lean execution

Compiler, CLI, migration, test, and offline-analysis code may need rich owned structures. Runtime execution should consume compact validated views and caller-owned state.

A common architecture is:

```text
allocating authoring/compiler layer
    -> validates, normalizes, sorts, packs, and serializes
immutable packed artifact
    -> borrowed by
a bounded, allocation-free runtime
```

Do not deserialize a packed artifact into a heap-resident object graph merely for convenience when the runtime can borrow validated slices directly.

### 1.5 Preserve a clear reference implementation

Optimized implementations MUST remain behaviorally equivalent to a clear, safe reference path. Keep the reference implementation readable enough to serve as:

- the normative semantics;
- a differential-testing oracle;
- a portability fallback;
- a basis for property tests;
- a reviewable specification for optimized code.

---

## 2. Memory and allocation contracts

Every crate, module, and performance-sensitive public operation MUST declare one of the following memory profiles.

| Profile | Contract | Typical use |
|---|---|---|
| **Strict heapless** | No heap-backed storage is used. Prefer `#![no_std]`; do not import `alloc`. | Core runtimes, embedded code, parsers and kernels requiring proof of no heap use. |
| **Allocation-free steady state** | Initialization may allocate, but the named operation and its complete transitive call graph perform zero allocation or reallocation after initialization. | Servers, reusable engines, prepared runtimes, per-request or per-token execution. |
| **Allocation-conscious** | Allocation is permitted only at explicit boundaries and must be justified, bounded where possible, and measured when performance-sensitive. | CLI, compilers, build tools, administrative services, offline analysis. |

The default for runtime, protocol, parser hot paths, deterministic kernels, and repeated request processing is **strict heapless** or **allocation-free steady state**.

### 2.1 Be precise about what is guaranteed

These claims are different:

- “This function does not call `Vec::new()`.”
- “This function performs no allocation on the exercised path.”
- “This function and every transitive callee perform no allocation for all valid inputs.”
- “This subsystem never uses heap-backed storage.”

Only the last statement is a strict no-heap guarantee.

A function accepting `&[T]` backed by a caller-created `Vec<T>` may itself be allocation-free, but the overall system is not heapless. A preallocated `Vec<T>` may satisfy a steady-state zero-allocation contract if it never grows, but it still uses heap memory and does not satisfy a strict heapless contract.

### 2.2 Allocation behavior is part of the API

Public runtime APIs SHOULD make allocation unnecessary and obvious from their signatures.

Prefer:

- `&T`, `&mut T`, `&[T]`, `&mut [T]`, and `&str`;
- caller-owned output buffers;
- caller-owned scratch buffers;
- fixed-size arrays for genuinely small bounds;
- fixed-capacity containers that cannot spill to the heap;
- borrowed views into validated bytes;
- iterators instead of collected results;
- returned lengths, ranges, and status values instead of owned collections;
- static dispatch or enum dispatch instead of boxed trait objects;
- small, copyable error enums.

Avoid signatures such as:

```rust
fn parse(input: String) -> Vec<Record>;
```

Prefer a shape such as:

```rust
fn parse(input: &[u8], output: &mut [Record]) -> Result<usize, ParseError>;
```

The second signature makes ownership, capacity, and failure behavior explicit and permits both heapless and heap-backed callers.

### 2.3 Do not move unbounded work onto the stack

Avoiding the heap does not mean placing arbitrarily large arrays on every thread stack.

Large storage SHOULD be:

- supplied by the caller;
- static when lifetime and synchronization permit;
- stored in a bounded arena with a documented lifecycle;
- memory-mapped;
- partitioned into bounded chunks;
- placed in a reusable worker-owned scratch region.

Review stack consumption against the smallest supported thread stack. Avoid unbounded recursion. Recursion is acceptable only when depth is statically or structurally bounded and documented.

### 2.4 Capacity exhaustion must be explicit

A fixed-capacity structure MUST report exhaustion deterministically.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapacityError {
    Full { capacity: usize },
}
```

It MUST NOT silently:

- spill into a heap allocation;
- discard existing entries;
- overwrite an unrelated entry;
- retry indefinitely;
- switch to an unbounded representation.

### 2.5 Allocation-free output-buffer pattern

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterError {
    OutputTooSmall {
        required: usize,
        provided: usize,
    },
}

/// Copies even values into `output` and returns the initialized prefix length.
///
/// # Allocation
///
/// This function performs no heap allocation.
pub fn retain_even(input: &[u32], output: &mut [u32]) -> Result<usize, FilterError> {
    let required = input
        .iter()
        .filter(|value| **value & 1 == 0)
        .count();

    if output.len() < required {
        return Err(FilterError::OutputTooSmall {
            required,
            provided: output.len(),
        });
    }

    // The release-active check above establishes this internal invariant.
    debug_assert!(required <= output.len());

    let mut written = 0usize;

    for &value in input {
        if value & 1 == 0 {
            debug_assert!(written < output.len());
            output[written] = value;
            written += 1;
        }
    }

    debug_assert_eq!(written, required);
    Ok(written)
}
```

The initial capacity branch is required in all builds because it handles caller-controlled input. The subsequent debug assertions verify internal invariants established by checked control flow.

### 2.6 Direct and hidden allocation to review

The following are forbidden in strict heapless code. They are also forbidden in an allocation-free operation unless they are constructed outside the operation and the exercised call path is proven not to grow, allocate, or reallocate:

- `Vec<T>`, `String`, `Box<T>`, `Rc<T>`, `Arc<T>`, `PathBuf`, `OsString`;
- `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `VecDeque`, `BinaryHeap`;
- `Box::new`, `Box::pin`, `Rc::new`, `Arc::new`;
- `vec![]`, `format!()`, `to_vec()`, allocating `to_owned()`, and `to_string()`;
- `collect::<Vec<_>>()`, `collect::<String>()`, and equivalent owned collections;
- growth through `push`, `insert`, `extend`, `reserve`, or implicit reallocation;
- boxed trait objects, boxed iterators, and boxed futures;
- `Cow::into_owned()` and APIs that may silently transition from borrowed to owned;
- spill-capable “small” containers unless spilling is structurally impossible;
- lazy initialization that creates owned heap data on first use;
- logging, tracing, metrics, serialization, backtrace, and error-context paths that have not been allocation-audited;
- callbacks, trait methods, FFI functions, or third-party dependencies whose transitive behavior is unknown.

Syntax alone does not determine allocation. Iterators, closures, formatting arguments, trait calls, and `async fn` are not inherently allocating, but a specific adapter, receiver, executor, or implementation may allocate. Review the complete call graph.

### 2.7 Formatting without owned strings

`format_args!()` creates borrowed formatting arguments without itself creating an owned `String`. The destination still determines whether formatting allocates.

Prefer writing directly to a caller-supplied or fixed-capacity sink:

```rust
use core::fmt::{self, Write as _};

pub fn write_record(
    sink: &mut impl fmt::Write,
    id: u32,
    score: i32,
) -> fmt::Result {
    write!(sink, "id={id} score={score}")
}
```

The function above is allocation-free only when the supplied sink is allocation-free. A `String` sink may grow; a fixed-capacity sink should return `fmt::Error` when full.

Avoid constructing owned diagnostic strings in runtime code. Defer rich formatting to a higher-level adapter after the core operation returns a typed error.

### 2.8 Sorting and selection

Where unstable ordering is acceptable, slice methods such as `sort_unstable*` and `select_nth_unstable*` are in-place and do not allocate. Do not replace a stable ordering requirement merely to avoid allocation; instead define the required semantics and choose or implement a bounded algorithm that satisfies them.

If deterministic output matters, explicitly define:

- total ordering;
- tie-breaking;
- treatment of equal keys;
- architecture-independent integer behavior;
- canonical output order.

### 2.9 Keep errors heapless at the core boundary

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    OffsetOutOfBounds {
        offset: usize,
        input_len: usize,
    },
    OutputTooSmall {
        required: usize,
        provided: usize,
    },
    CapacityExceeded {
        capacity: usize,
    },
    IntegerOverflow,
}
```

Implement `Display` by writing directly to the formatter. Do not store a preformatted `String` merely to add context. Application code may translate a core error into a richer allocating diagnostic after crossing the allocation-free boundary.

### 2.10 Isolate strict code structurally

A strict core crate SHOULD begin from a posture similar to:

```rust
#![no_std]
#![forbid(unsafe_code)]
#![deny(clippy::disallowed_macros)]
#![deny(clippy::disallowed_types)]
```

Do not add `extern crate alloc` to a crate claiming strict no-heap behavior. Put filesystem, network, CLI, telemetry, and rich diagnostic integrations in adapter crates.

A recommended workspace shape is:

```text
crates/
  project-core/       # no_std, no alloc, bounded algorithms
  project-format/     # packed types and validation
  project-runtime/    # allocation-free repeated execution
  project-std/        # std adapters, I/O, threading, telemetry
  project-cli/        # allocating application boundary
```

---

## 3. Ownership, borrowing, and values

### 3.1 Borrow instead of clone by default

Take borrowed inputs unless ownership transfer is required.

Prefer:

```rust
fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, byte| sum.wrapping_add(u32::from(*byte)))
}
```

Avoid:

```rust
fn checksum(bytes: Vec<u8>) -> u32 {
    // Ownership was unnecessary.
    bytes.iter().fold(0u32, |sum, byte| sum.wrapping_add(u32::from(*byte)))
}
```

Use:

- `&str`, not `&String`;
- `&[T]`, not `&Vec<T>`;
- `&Path`, not `&PathBuf`;
- borrowed domain views rather than cloned domain objects.

A clone is appropriate when independent ownership is semantically required. When cloning, make the cost visible and intentional. Avoid cloning to satisfy the borrow checker before understanding the ownership model.

### 3.2 Pass small `Copy` values by value

Pass scalar values, compact newtypes, and small `Copy` structs by value when that is clearer. Do not establish a universal byte threshold without measuring the relevant ABI and target.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Range32 {
    pub start: u32,
    pub end: u32,
}

fn contains(range: Range32, value: u32) -> bool {
    value >= range.start && value < range.end
}
```

Large structs and non-`Copy` values should usually be borrowed unless the function consumes them intentionally.

### 3.3 Make ownership transitions obvious

Names such as `into_*`, `to_*`, and `as_*` should follow Rust conventions:

- `into_*`: consumes `self`;
- `to_*`: usually creates or converts to an owned value and may allocate;
- `as_*`: returns a borrowed or inexpensive view.

Document allocation when a conversion creates owned storage.

### 3.4 Avoid self-referential and pointer-heavy designs without need

Prefer contiguous data and index-based relationships over linked object graphs. Indexes and offsets are easier to:

- serialize;
- validate;
- borrow;
- cache efficiently;
- bound;
- move across FFI;
- execute without allocation.

Use pointer-rich structures only when their semantics and measured workload justify the complexity.

---

## 4. `Option`, `Result`, panics, and arithmetic

### 4.1 Use `Option` for absence and `Result` for failure

Do not encode absence with empty strings, zero IDs, negative numbers, or invalid pointers.

```rust
pub fn record_at(records: &[Record], index: usize) -> Result<&Record, LookupError> {
    let record = records.get(index).ok_or(LookupError::OutOfBounds {
        index,
        len: records.len(),
    })?;

    debug_assert!(index < records.len());
    Ok(record)
}
```

Use combinators when they improve clarity. Use `match`, `if let`, or `let ... else` when control flow or error context is clearer explicitly.

### 4.2 Avoid `unwrap()` and `expect()` in production paths

Production libraries MUST NOT use `unwrap()` or `expect()` for recoverable conditions. Tests, examples specifically demonstrating panic behavior, and compile-time-proven constants may use them sparingly, but even there a clearer assertion is often better.

Do not convert a recoverable error into a panic merely because handling it is inconvenient.

### 4.3 Prefer typed library errors

Library errors should be focused, stable, and domain-specific. Do not expose a third-party error type as the core public contract unless that coupling is intentional.

At a rich application boundary, additional context may be attached. In strict or allocation-free code, error context must remain allocation-free.

`thiserror` may be useful for allocating or `std`-facing crates when compatible with the project’s MSRV and feature policy. `anyhow` is appropriate at binary/application boundaries, not in core library APIs or strict heapless code.

### 4.4 Use `?` for propagation without hiding policy

The `?` operator is preferred for straightforward propagation. Do not use it to obscure meaningful translation, retry, rollback, or cleanup policy.

```rust
let header = parse_header(input)?;
let body = parse_body(input, header.body_range)
    .map_err(ParsePacketError::Body)?;
```

### 4.5 Panic only for genuine programmer defects

Caller-controlled input, malformed artifacts, capacity exhaustion, I/O failures, and unavailable resources are not programmer defects. Return a typed error.

A panic MAY be appropriate when an internal invariant is violated and continuing would indicate a defect. In libraries, keep panic conditions rare and documented under `# Panics`.

### 4.6 Use checked arithmetic at trust boundaries

Offsets, lengths, capacities, and serialized values MUST use checked arithmetic before indexing or allocation decisions.

```rust
let end = start
    .checked_add(length)
    .ok_or(ParseError::IntegerOverflow)?;

if end > input.len() {
    return Err(ParseError::OffsetOutOfBounds {
        offset: end,
        input_len: input.len(),
    });
}

debug_assert!(start <= end);
debug_assert!(end <= input.len());
```

Choose and document overflow semantics for domain arithmetic:

- checked and erroring;
- saturating;
- wrapping;
- explicitly proven impossible.

Do not rely on debug-only overflow behavior as the release contract.

---

## 5. `debug_assert!()` and internal invariants

### 5.1 Use debug assertions deliberately

Use `debug_assert!()`, `debug_assert_eq!()`, and `debug_assert_ne!()` for internal invariants when all of the following are true:

1. The condition is established by types or release-active control flow.
2. The condition represents a programming invariant, not caller validation.
3. Release correctness does not depend on the assertion executing.
4. Removing the assertion cannot introduce undefined behavior.
5. Evaluating the assertion has no required side effects.
6. The condition and message do not intentionally allocate.

Good examples:

```rust
debug_assert!(cursor <= input.len());
debug_assert!(written <= output.len());
debug_assert_eq!(range.end - range.start, record_count);
debug_assert_ne!(capacity, 0);
```

Use debug assertions after important state transitions, checked bounds calculations, fixed-capacity writes, parser cursor movement, and canonicalization steps when they provide meaningful defect detection.

Do not add assertions mechanically. Every assertion should communicate a real invariant.

### 5.2 Validate external input in all builds

Incorrect:

```rust
pub fn read_byte(input: &[u8], index: usize) -> u8 {
    debug_assert!(index < input.len());
    input[index]
}
```

The function accepts caller-controlled input but provides no recoverable contract.

Prefer:

```rust
pub fn read_byte(input: &[u8], index: usize) -> Result<u8, LookupError> {
    let value = input.get(index).copied().ok_or(LookupError::OutOfBounds {
        index,
        len: input.len(),
    })?;

    debug_assert!(index < input.len());
    Ok(value)
}
```

### 5.3 Never use a debug assertion as an unsafe precondition

Incorrect:

```rust
// Incorrect: the bounds proof normally disappears in optimized builds.
debug_assert!(index < values.len());
let value = unsafe { *values.get_unchecked(index) };
```

Unsafe code MUST rely on:

- types that enforce the requirement;
- release-active validation;
- a documented invariant proven independently of debug assertions.

A debug assertion may duplicate a valid proof for diagnostics, but it cannot be the proof.

### 5.4 Assertions must not contain required side effects

Incorrect:

```rust
// The state update may disappear in optimized builds.
debug_assert!(advance_cursor(&mut cursor));
```

Correct:

```rust
let advanced = advance_cursor(&mut cursor);
debug_assert!(advanced);
```

The program must behave correctly whether debug assertions are enabled or disabled.

### 5.5 Keep assertion diagnostics allocation-aware

Prefer simple conditions and static messages:

```rust
debug_assert!(written <= capacity, "written length exceeded capacity");
```

Avoid constructing owned values:

```rust
// Avoid in allocation-free code.
debug_assert!(is_valid(&input.to_vec()));
debug_assert!(ok, "state={}", state.to_string());
```

`debug_assert_eq!()` and `debug_assert_ne!()` format values with `Debug` when they fail. Ensure custom `Debug` implementations do not create owned strings in strict paths.

### 5.6 Do not count invariant-panic behavior as a recoverable path

Panic hooks, backtraces, and diagnostic output may allocate depending on the target and configuration. Unless the panic path is separately audited, an allocation-free guarantee should cover:

- successful execution;
- all documented recoverable error paths;
- capacity exhaustion;
- malformed external input handling.

An internal invariant panic is a defect path, not an ordinary result path.

### 5.7 Test optimized code with debug assertions enabled

Add a release-like profile:

```toml
[profile.release-assertions]
inherits = "release"
debug-assertions = true
overflow-checks = true
```

Run it in CI:

```bash
cargo test --workspace --profile release-assertions
```

This catches invariants under optimized control flow while preserving a separate normal release profile.

---

## 6. Iterators, loops, and collection behavior

### 6.1 Iterators are not inherently allocating

Iterator adapters are generally lazy. Allocation usually occurs when collecting into an owned container or when a particular adapter or closure performs allocation.

Prefer a borrowed iterator when callers can consume results incrementally:

```rust
pub fn active_records(
    records: &[Record],
) -> impl Iterator<Item = &Record> {
    records.iter().filter(|record| record.active)
}
```

Do not collect solely to return a convenient intermediate `Vec`.

### 6.2 Use `for` loops when they are clearer

A direct loop is often the clearest form for:

- multiple mutable accumulators;
- explicit bounds and capacity checks;
- early exits;
- state machines;
- hot kernels whose generated code is inspected.

```rust
let mut written = 0usize;
for item in input {
    if keep(item) {
        if written == output.len() {
            return Err(CapacityError::Full {
                capacity: output.len(),
            });
        }

        debug_assert!(written < output.len());
        output[written] = *item;
        written += 1;
    }
}
```

Choose the form that makes correctness and bounds easiest to review. Do not rewrite readable loops into complex iterator chains merely to appear idiomatic.

### 6.3 Avoid intermediate collections

Instead of:

```rust
let normalized: Vec<_> = input.iter().map(normalize).collect();
let selected: Vec<_> = normalized.iter().filter(|value| accept(value)).collect();
```

Use a fused iterator or caller-owned output:

```rust
for value in input.iter().map(normalize).filter(accept) {
    // Consume immediately or place into bounded caller-owned storage.
}
```

### 6.4 Do not assume a closure is free

A closure can capture owned state, clone data, call allocating code, or force dynamic dispatch. Review captures and generated types in hot paths.

Prefer borrowing captures where possible. Use `move` only when ownership transfer is required.

---

## 7. Function and module design

### 7.1 Keep functions single-purpose

Extract a function when it creates a meaningful semantic boundary, improves testing, centralizes an invariant, or removes duplicated policy.

Do not extract tiny fragments that:

- obscure a simple control flow;
- require many pass-through parameters;
- hide performance-critical work;
- make ownership harder to understand;
- create abstractions with no stable meaning.

### 7.2 Keep hot paths easy to inspect

Performance-sensitive loops SHOULD make these properties visible:

- bounds;
- memory access pattern;
- temporary state;
- allocation behavior;
- error exits;
- branch structure;
- ordering and tie-breaking.

Abstraction is welcome when it compiles cleanly and preserves visibility. Keep a benchmark and reference implementation when the optimized path becomes non-obvious.

### 7.3 Avoid boolean blindness

Instead of:

```rust
fn execute(strict: bool, retry: bool, audit: bool) -> Result<(), Error>;
```

Prefer:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationMode {
    Strict,
    Compatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionPolicy {
    pub validation: ValidationMode,
    pub retry: RetryPolicy,
    pub audit: AuditPolicy,
}
```

### 7.4 Keep imports explicit

Avoid wildcard imports outside controlled preludes and test modules. Import traits intentionally when needed for extension methods.

Group imports consistently and let `rustfmt` determine formatting. Do not use aliases that obscure well-known types unless resolving a real collision or expressing domain meaning.

### 7.5 Keep visibility narrow

Start private. Expand to `pub(crate)` or `pub` only when a real consumer requires it. Public APIs create compatibility obligations.

Avoid exposing implementation collections, synchronization primitives, or third-party types if callers do not need them.

---

## 8. Generics, static dispatch, dynamic dispatch, and async

### 8.1 Prefer static dispatch in hot and strict paths

Generics and `impl Trait` allow specialization without requiring a heap allocation or virtual call.

```rust
pub fn encode<W: ByteSink>(sink: &mut W, value: &Record) -> Result<(), W::Error> {
    // ...
    Ok(())
}
```

Use static dispatch when:

- the set of implementations is known at compile time;
- performance matters;
- the code belongs to a strict allocation-free core;
- monomorphization cost is acceptable.

### 8.2 Use `dyn Trait` only for genuine runtime polymorphism

A borrowed `&dyn Trait` does not itself require heap allocation. A `Box<dyn Trait>` does.

Dynamic dispatch is appropriate when:

- implementations are selected at runtime;
- heterogeneous values must be stored behind one interface;
- reducing code size matters more than virtual-call overhead;
- the dynamic boundary is outside the hot path.

Prefer enum dispatch when the implementation set is closed and small.

### 8.3 Put allocation at an explicit outer boundary

A control plane may choose a backend dynamically while the chosen backend runs statically inside a hot operation. Do not force all internal APIs to use boxed trait objects merely because one outer layer needs runtime selection.

### 8.4 Async syntax does not prove allocation freedom

An `async fn` returns a future value and does not inherently require a box. Allocation may still occur through:

- boxed futures;
- dynamic async trait adapters;
- task spawning;
- executor task storage;
- channels and queues;
- captured owned buffers;
- I/O libraries;
- telemetry.

An allocation-free async claim must include the executor, spawning policy, adapters, and full call graph.

### 8.5 Bound concurrency

Concurrency MUST NOT multiply memory usage without a bound.

Define:

- maximum workers;
- queue capacities;
- per-worker scratch usage;
- backpressure behavior;
- cancellation behavior;
- deterministic merge ordering where output is canonical.

Prefer worker-local reusable scratch buffers over shared `Mutex<Vec<_>>` accumulation. Avoid assigning semantic IDs through scheduling-dependent atomics when deterministic output matters.

---

## 9. Type-state and lifecycle safety

Use type-state when legal operations depend strongly on lifecycle state and the additional types make the API clearer.

```rust
use core::marker::PhantomData;

pub struct Unvalidated;
pub struct Validated;

pub struct Artifact<'a, State> {
    bytes: &'a [u8],
    _state: PhantomData<State>,
}

impl<'a> Artifact<'a, Unvalidated> {
    pub fn validate(self) -> Result<Artifact<'a, Validated>, ValidationError> {
        validate_bytes(self.bytes)?;

        Ok(Artifact {
            bytes: self.bytes,
            _state: PhantomData,
        })
    }
}

impl Artifact<'_, Validated> {
    pub fn records(&self) -> RecordIter<'_> {
        debug_assert!(header_is_valid(self.bytes));
        RecordIter::new(self.bytes)
    }
}
```

The debug assertion above checks a property already established by construction. It must not be the only validation.

Use type-state for a small number of meaningful states. Avoid creating an explosion of generic parameters for incidental flags. When state is dynamic, externally supplied, or persisted, a runtime enum may be clearer.

---

## 10. Pointers, sharing, concurrency, and unsafe Rust

### 10.1 Prefer references and slices

Use references for borrowing and slices for contiguous data. Reach for smart pointers only when their ownership semantics are actually required.

Remember:

- `Box<T>` owns heap storage;
- `Rc<T>` owns heap storage with non-atomic reference counting;
- `Arc<T>` owns heap storage with atomic reference counting;
- cloning `Rc` or `Arc` may not allocate, but the value remains heap-backed and therefore is not strict heapless;
- interior mutability changes aliasing and synchronization reasoning.

### 10.2 Do not use shared ownership as a default escape hatch

Frequent `Arc<Mutex<T>>` use can signal unclear ownership. Prefer:

- a single explicit owner;
- message passing with bounded queues;
- immutable shared data;
- scoped threads borrowing state;
- partitioned state;
- IDs or handles into a controlled store.

Use locks when they are the clearest correct design. Lock-free code is not automatically faster or safer.

### 10.3 Treat `Send` and `Sync` as semantic commitments

Do not add unsafe `Send` or `Sync` implementations without a written proof covering all interior state, aliases, callbacks, and FFI interactions.

### 10.4 Forbid unsafe code by default

Use:

```rust
#![forbid(unsafe_code)]
```

in crates that do not require unsafe Rust.

When unsafe code is necessary:

- isolate it in the smallest possible module;
- expose a safe API;
- document every unsafe block with a `SAFETY:` comment;
- state all pointer, alignment, initialization, aliasing, lifetime, and concurrency invariants;
- enforce preconditions in release-active code or types;
- add focused tests and Miri coverage where applicable;
- retain a safe reference implementation when optimizing;
- never rely solely on `debug_assert!()` for safety.

### 10.5 Audit FFI as a complete boundary

FFI documentation MUST define:

- ownership transfer;
- who allocates and deallocates;
- allocator compatibility;
- pointer validity and alignment;
- buffer length and capacity;
- lifetime;
- thread affinity;
- panic behavior;
- error representation;
- callback reentrancy.

Rust panics must not unwind across an FFI boundary unless the ABI explicitly supports and documents it.

---

## 11. Performance mindset

### 11.1 Measure before and after

Do not optimize based only on intuition. Establish:

- a representative workload;
- release-mode measurements;
- hardware and compiler metadata;
- latency distribution, not only averages;
- throughput;
- peak memory;
- allocation counts;
- bytes processed;
- cache behavior when relevant;
- regression thresholds.

### 11.2 Improve algorithms and data layout first

Prioritize:

1. asymptotic behavior;
2. bounded candidate sets;
3. avoiding unnecessary work;
4. contiguous data layout;
5. reducing bytes read and written;
6. avoiding allocation and copies;
7. cache locality;
8. branch predictability;
9. vectorization or architecture-specific kernels only after the above.

A reduction in arithmetic count is not necessarily a speedup if memory traffic, cache misses, or synchronization dominate.

### 11.3 Avoid redundant clones and copies

Use profiling and code review to identify:

- cloning inside loops;
- copying large structs by value;
- converting repeatedly between string and byte representations;
- serializing only to deserialize immediately;
- collecting intermediate results;
- copying buffers across abstraction boundaries.

Do not remove a clone if doing so makes ownership unsound or materially harms clarity. Fix the ownership model rather than introducing fragile references.

### 11.4 Reuse prepared state

For steady-state allocation-free systems:

- validate and prepare once;
- precompute immutable tables;
- allocate permitted capacity during initialization;
- reuse worker-local scratch;
- reset lengths and cursors rather than reconstructing containers;
- keep repeated operations free of lazy initialization.

### 11.5 Inspect generated code selectively

Assembly or LLVM IR inspection is useful for critical kernels, especially when verifying:

- bounds-check elimination;
- vectorization;
- unexpected calls;
- hidden allocation;
- integer operations;
- branch structure;
- architecture-specific instruction use.

Generated code inspection supplements behavioral tests; it does not replace them.

### 11.6 Keep debug assertions in a dedicated optimized test lane

Normal release benchmarks should reflect production settings. Separately run optimized tests with `debug-assertions = true` so invariant checks execute under optimized code generation.

---

## 12. Clippy, formatting, and lint discipline

### 12.1 Treat warnings as failures in CI

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

If workspace features are mutually exclusive, replace `--all-features` with an explicit tested feature matrix.

### 12.2 Configure workspace lints centrally

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
unused_must_use = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
dbg_macro = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unwrap_used = "deny"
```

Each member crate adopts the workspace policy:

```toml
[lints]
workspace = true
```

Do not enable a broad lint set blindly and then scatter suppressions. Tune the policy to the codebase and MSRV.

### 12.3 Fix warnings instead of hiding them

Prefer `#[expect(...)]` with a reason when the project MSRV supports it. Otherwise use the narrowest possible `#[allow(...)]` and explain why.

A suppression MUST be:

- local;
- tied to a specific lint;
- justified;
- removable when the underlying constraint changes.

Do not disable a lint at workspace scope to avoid fixing one call site.

### 12.4 Add allocation guardrails for strict crates

A workspace can reject obvious heap-backed types and macros:

```toml
# clippy.toml

disallowed-types = [
  { path = "alloc::boxed::Box", reason = "heap allocation is forbidden in strict runtime code", allow-invalid = true },
  { path = "alloc::string::String", reason = "borrow text or use fixed-capacity storage", allow-invalid = true },
  { path = "alloc::vec::Vec", reason = "use caller-owned slices or fixed-capacity storage", allow-invalid = true },
  { path = "std::boxed::Box", reason = "heap allocation is forbidden in strict runtime code", allow-invalid = true },
  { path = "std::string::String", reason = "borrow text or use fixed-capacity storage", allow-invalid = true },
  { path = "std::vec::Vec", reason = "use caller-owned slices or fixed-capacity storage", allow-invalid = true },
]

disallowed-macros = [
  { path = "alloc::format", reason = "write into a caller-owned or fixed-capacity sink", allow-invalid = true },
  { path = "alloc::vec", reason = "use arrays, slices, or fixed-capacity storage", allow-invalid = true },
  { path = "std::format", reason = "write into a caller-owned or fixed-capacity sink", allow-invalid = true },
  { path = "std::vec", reason = "use arrays, slices, or fixed-capacity storage", allow-invalid = true },
]
```

Enable the corresponding deny lints in strict crates:

```rust
#![deny(clippy::disallowed_macros)]
#![deny(clippy::disallowed_types)]
```

This is a guardrail, not a proof. It cannot see allocation hidden behind custom types, dependencies, callbacks, FFI, logging, or trait methods.

### 12.5 Keep formatting mechanical

Use `rustfmt`. Do not spend review time debating formatting that the formatter owns. Keep manual style decisions focused on naming, module shape, visibility, API semantics, and control flow.

---

## 13. Automated testing

### 13.1 Tests are executable documentation

Tests should demonstrate behavior, boundaries, and failure policy. A test name should describe the condition and expected result.

```rust
#[test]
fn parse_returns_output_too_small_when_capacity_is_insufficient() {
    // ...
}
```

Prefer one behavioral reason for failure per test. Multiple assertions are fine when they jointly establish that one behavior.

### 13.2 Use the right test layer

- **Unit tests**: local algorithms, invariants, edge cases, error variants.
- **Integration tests**: public APIs, crate boundaries, feature combinations, allocation contracts.
- **Doc tests**: public usage examples that should continue compiling.
- **Property tests**: broad invariant exploration.
- **Fuzz tests**: parsers, protocol decoders, unsafe boundaries, malformed input.
- **Differential tests**: optimized implementation versus safe reference implementation.
- **Concurrency model tests**: ordering and synchronization behavior when needed.

### 13.3 Test boundaries, not only happy paths

Allocation-free and bounded APIs MUST test:

- empty input;
- one element;
- exact capacity;
- one less than required capacity;
- maximum declared capacity;
- malformed offsets and lengths;
- integer overflow boundaries;
- duplicate and equal-key behavior;
- deterministic tie-breaking;
- repeated warm execution;
- every recoverable error variant;
- optional instrumentation paths;
- feature-disabled configurations.

### 13.4 Test debug assertions intentionally

Ordinary debug tests execute debug assertions. Also run the release-like assertion profile:

```bash
cargo test --workspace --profile release-assertions
```

When an invariant should panic in debug mode, isolate that behavior in a focused test rather than relying on an incidental panic in a broad test.

### 13.5 Count allocations in a dedicated integration test

The test-only allocator wrapper below contains a narrow `unsafe` boundary because `GlobalAlloc` is unsafe to implement. It is not part of production code.

```rust
// tests/no_alloc.rs

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator;

static ALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn allocation_calls() -> usize {
    ALLOCATION_CALLS.load(Ordering::Relaxed)
}

fn deallocation_calls() -> usize {
    DEALLOCATION_CALLS.load(Ordering::Relaxed)
}

#[test]
fn retain_even_performs_no_heap_activity() {
    let input = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut output = [0u32; 4];

    // Complete all fixture and runtime setup before capturing the baseline.
    let allocations_before = allocation_calls();
    let deallocations_before = deallocation_calls();

    // Replace this path with the allocation-free API being certified.
    let result = crate_under_test::retain_even(&input, &mut output);

    let allocations_after = allocation_calls();
    let deallocations_after = deallocation_calls();

    assert_eq!(allocations_before, allocations_after);
    assert_eq!(deallocations_before, deallocations_after);
    assert_eq!(result, Ok(4));
    assert_eq!(output, [2, 4, 6, 8]);
}
```

Run the target alone and serially:

```bash
cargo test --test no_alloc -- --test-threads=1
```

Allocation-counting tests prove only the paths and inputs they exercise. Combine them with:

- strict API review;
- dependency and feature review;
- `no_std` / no-`alloc` compilation where applicable;
- malformed-input tests;
- capacity tests;
- call-graph inspection;
- Clippy guardrails.

### 13.6 Keep snapshots reviewable

Snapshot tests are useful for structured diagnostics, generated code, canonical serialization, and CLI output. Keep snapshots small, deterministic, and human-reviewable. Do not use snapshots as a substitute for precise semantic assertions.

### 13.7 Test documentation

Public examples SHOULD be doc tests when practical. Documentation that compiles is less likely to drift.

---

## 14. Documentation and comments

### 14.1 Comments explain why

Use comments for:

- non-obvious invariants;
- algorithmic rationale;
- compatibility constraints;
- safety proofs;
- measured performance tradeoffs;
- protocol or specification references;
- reasons a simpler-looking implementation is incorrect.

Do not narrate obvious syntax.

Bad:

```rust
// Increment the index.
index += 1;
```

Useful:

```rust
// Advance only after the record has been committed so an error leaves the
// caller-visible prefix unchanged.
index += 1;
```

### 14.2 Public documentation explains the contract

Public APIs SHOULD document applicable sections:

- purpose and semantics;
- inputs and outputs;
- ownership and lifetimes;
- allocation behavior under `# Allocation`;
- finite limits and capacity behavior;
- `# Errors`;
- `# Panics`;
- `# Safety` for unsafe APIs;
- determinism and ordering;
- complexity when meaningful;
- examples.

Example:

```rust
/// Parses records into caller-owned storage.
///
/// # Allocation
///
/// Performs no heap allocation. Temporary state is held in scalar locals and
/// the caller-provided `output` slice.
///
/// # Errors
///
/// Returns [`ParseError::OutputTooSmall`] without modifying elements beyond
/// the returned initialized prefix.
///
/// # Panics
///
/// Does not panic for malformed input.
```

### 14.3 Keep TODOs traceable

Every committed TODO SHOULD reference an issue or decision:

```rust
// TODO(#421): Replace the scalar verifier after SIMD equivalence tests exist.
```

Do not leave vague TODOs that have no owner, scope, or removal condition.

### 14.4 Replace stale comments with code or types

If a comment describes a requirement that can be enforced by a type, constructor, enum, validation step, or test, prefer enforcement. Comments are not proofs.

---

## 15. Dependencies, features, and workspace boundaries

### 15.1 Minimize the strict dependency graph

Core runtime crates SHOULD have the smallest practical dependency surface. Every dependency can introduce:

- allocation;
- feature unification;
- platform assumptions;
- unsafe code;
- build scripts;
- transitive vulnerabilities;
- larger binaries;
- MSRV pressure.

Do not add a dependency for a trivial helper that is clearer to implement locally.

### 15.2 Disable default features intentionally

Inspect dependency features rather than accepting defaults automatically:

```toml
[dependencies]
some-crate = { version = "1", default-features = false, features = ["required-feature"] }
```

Test the intended feature matrix, especially:

```bash
cargo check -p project-core --no-default-features
cargo check -p project-core --no-default-features --features feature_a
```

A feature enabled elsewhere in a workspace can change the unified dependency graph. Audit the resolved graph, not only one manifest entry.

### 15.3 Separate core and adapters

Do not make a heapless core depend on CLI parsing, async runtimes, telemetry, filesystem abstractions, HTTP clients, or rich error-reporting frameworks. Put those integrations in outer crates.

### 15.4 Pin and audit appropriately

Keep `Cargo.lock` committed for applications and workspaces. Use dependency, license, and vulnerability auditing appropriate to the project. Review build scripts and proc macros as supply-chain code.

### 15.5 Maintain a documented MSRV when promised

If the project promises a minimum supported Rust version, test it in CI. Do not use new syntax, attributes, or library APIs without either updating the MSRV intentionally or providing a compatible alternative.

---

## 16. Determinism and canonical output

When output is content-addressed, signed, cached, compared byte-for-byte, or used as a reproducibility artifact, determinism is a correctness requirement.

Define and test:

- input ordering;
- stable IDs;
- sorting and tie-breaking;
- hash-map independence;
- random seed policy;
- concurrency merge order;
- integer overflow semantics;
- architecture-independent widths and endianness;
- serialization field order;
- canonical padding and alignment;
- error ordering where multiple failures are possible.

Parallel execution MAY change completion time but MUST NOT change canonical bytes when determinism is part of the contract.

Use debug assertions for internal canonicalization invariants after the release-active algorithm has established them:

```rust
canonicalize(records)?;

debug_assert!(records.windows(2).all(|pair| pair[0].key <= pair[1].key));
```

Do not rely on iteration order from a container unless that order is explicitly guaranteed and appropriate for the artifact format.

---

## 17. Recommended Cargo and CI baseline

### 17.1 Cargo profiles

```toml
[profile.release]
overflow-checks = true

[profile.release-assertions]
inherits = "release"
debug-assertions = true
overflow-checks = true
```

Choose LTO, codegen units, panic strategy, and symbol stripping based on measured build, binary-size, diagnostics, and deployment requirements. Do not copy a profile blindly across every crate and target.

### 17.2 Required checks

A strong baseline is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --doc --locked
cargo test --workspace --profile release-assertions
cargo test --test no_alloc -- --test-threads=1
```

Add as applicable:

```bash
cargo check -p project-core --no-default-features
cargo miri test -p project-core
cargo test --workspace --release
cargo audit
cargo deny check
```

If `--all-features` represents an invalid combination, use an explicit feature matrix instead of skipping feature coverage.

### 17.3 Performance gates

Benchmarks used as merge gates MUST have:

- stable fixtures;
- known warmup behavior;
- hardware metadata;
- noise-aware thresholds;
- allocation counters where relevant;
- separate correctness tests;
- recorded baseline changes.

Do not make fragile microbenchmark noise a hard correctness gate.

---

## 18. Code review checklist

### Correctness and API

- Are invalid states represented with types rather than conventions?
- Are caller-controlled failures returned as typed errors?
- Are integer and range calculations checked before indexing?
- Are panic conditions rare and documented?
- Is ownership transfer intentional and visible?
- Are public fields and types truly required?

### Allocation and bounds

- Is the memory profile declared?
- Do hot/repeated operations avoid heap allocation and reallocation?
- Are inputs borrowed and outputs or scratch buffers caller-owned where practical?
- Are all capacities finite and documented?
- Does exhaustion return an explicit error without spilling to the heap?
- Have hidden paths through formatting, telemetry, async, traits, callbacks, FFI, and dependencies been reviewed?
- Are large buffers kept off limited thread stacks?

### `debug_assert!()`

- Does each debug assertion express a real internal invariant?
- Is the invariant already established by types or release-active checks?
- Would release behavior remain correct if the assertion were removed?
- Is the assertion free of required side effects?
- Is it independent of unsafe-code soundness?
- Does the condition or message avoid owned allocation?

### Performance and determinism

- Is optimization supported by measurement?
- Is data layout contiguous and cache-conscious where relevant?
- Are unnecessary clones, copies, and intermediate collections avoided?
- Is canonical output independent of thread scheduling and unordered containers?
- Does an optimized implementation have a reference oracle and equivalence tests?

### Safety and concurrency

- Is unsafe code forbidden or tightly isolated?
- Does every unsafe block have a complete `SAFETY:` explanation?
- Are FFI ownership and allocator rules explicit?
- Are queues, workers, retries, and temporary storage bounded?
- Is shared ownership used intentionally rather than as an ownership escape hatch?

### Tests and documentation

- Are success, capacity, malformed-input, and error paths tested?
- Is allocation behavior tested after initialization?
- Are optimized tests run with debug assertions enabled?
- Do public APIs document allocation, errors, panics, bounds, and determinism?
- Are TODOs linked to issues?
- Do examples compile as doc tests where practical?

---

## 19. Common anti-patterns

Avoid these patterns unless a documented exception explains why they are correct:

- cloning to silence the borrow checker;
- accepting `String` or `Vec<T>` when only a borrow is needed;
- returning a `Vec<T>` from every query or parser;
- using `unwrap()` for external input;
- using `debug_assert!()` as validation or a safety precondition;
- putting state changes inside debug assertions;
- using `format!()` in a strict runtime error path;
- preallocating a `Vec` and calling the subsystem “heapless”;
- relying on a small-vector type that may spill to the heap;
- hiding allocations behind logging or error context;
- boxing futures or traits in a hot path without measuring or documenting it;
- using unbounded channels, retries, recursion, or task creation;
- assigning canonical IDs based on thread completion order;
- replacing clear code with an abstraction that obscures bounds and memory access;
- writing unsafe code before proving safe code is insufficient;
- disabling lints globally to accommodate one call site;
- claiming allocation freedom based only on source inspection or one benchmark input.

---

## 20. Final standard

Production Rust should be easy to reason about under both success and failure. The preferred design has:

- explicit domain types;
- borrowed inputs;
- caller-owned or fixed-capacity output and scratch storage;
- bounded execution;
- typed, allocation-free core errors;
- release-active validation of external input;
- `debug_assert!()` checks for internal invariants already established by correct code;
- no unsafe code by default;
- deterministic output where artifacts or proofs depend on it;
- measurement-backed optimization;
- tests that verify allocation behavior and boundary conditions;
- rich application adapters separated from a lean runtime core.

A zero-allocation claim is a contract. Treat it with the same rigor as memory safety, wire-format compatibility, and deterministic output.

---

## References

- Rust API Guidelines: <https://rust-lang.github.io/api-guidelines/>
- Rust Style Guide: <https://doc.rust-lang.org/stable/style-guide/>
- Rust `debug_assert!()` documentation: <https://doc.rust-lang.org/std/macro.debug_assert.html>
- Rust `assert!()` documentation: <https://doc.rust-lang.org/std/macro.assert.html>
- Rust `no_std` documentation: <https://doc.rust-lang.org/stable/std/attribute.no_std.html>
- Rust `alloc` crate documentation: <https://doc.rust-lang.org/stable/alloc/>
- Rust `format_args!()` documentation: <https://doc.rust-lang.org/std/macro.format_args.html>
- Rust slice methods, including allocation-free unstable sorting: <https://doc.rust-lang.org/stable/core/primitive.slice.html>
- Cargo profile reference: <https://doc.rust-lang.org/cargo/reference/profiles.html>
- Clippy configuration: <https://doc.rust-lang.org/nightly/clippy/configuration.html>
- Clippy lint configuration options: <https://doc.rust-lang.org/nightly/clippy/lint_configuration.html>
- Apollo GraphQL Rust Programming Best Practices Handbook: <https://github.com/apollographql/rust-best-practices>
