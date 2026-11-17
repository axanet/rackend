# 0001. Object-safe `Store` port with a typed extension trait

- Status: accepted
- Date: 2026-11-17
- Spec: §4.3, §4.4

## Context
SPEC §4.4 shows `Store::read<R>` / `Store::write<R>` as generic async methods taking closures. A trait with generic methods is not object safe, so every use case, handler and the HTTP state would have to be generic over the concrete store type.

## Decision
The `Store` trait in `core::ports` moves type-erased jobs (`Box<dyn FnOnce(&mut dyn WriteTx) -> Result<Box<dyn Any + Send>>>`) and returns a boxed future, so it can be used as `Arc<dyn Store>`. A blanket extension trait `StoreExt` provides exactly the typed API of §4.4 (`store.read(|tx| ...)`, `store.write(|tx| ...)`) and downcasts the result.

## Consequences
- Use cases and adapters hold `Arc<dyn Store>`; no generic plumbing through axum.
- One allocation and one downcast per transaction: negligible next to an SQLite transaction.
- A wrong downcast is impossible through `StoreExt` (the type is fixed by the closure) and maps to `Error::Internal` defensively.
