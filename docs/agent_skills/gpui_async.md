---
name: gpui-async
description: Guidelines for async programming in GPUI, covering native executors (smol) and Tokio integration via gpui_tokio, with patterns
---

This document describes the async patterns used in GPUI for agent development.

## Core Async Pattern

The fundamental pattern for async operations in GPUI entities:

```rust
struct MyView {
    _async_task: Option<Task<()>>,
    state: String,
}

impl MyView {
    fn perform_async_work(&mut self, cx: &mut Context<Self>) {
        // Store task to prevent cancellation when dropped
        self._async_task = Some(cx.spawn(async move |this, cx| {
            // Perform async work
            cx.background_executor().timer(Duration::from_secs(1)).await;
            
            // Update state synchronously on foreground thread
            this.update(cx, |this, cx| {
                this.state = "updated".to_string();
                cx.notify(); // Trigger re-render
            }).ok();
        }));
    }
}
```

**Key elements:**
- Store `Task` in struct field as `Option<Task<()>>` to prevent cancellation
- Use `cx.spawn(async move |this, cx| ...)` where `this: WeakEntity<T>` and `cx: &mut AsyncApp` 
- Update state via `this.update(cx, |this, cx| ...)` to mutate entity state synchronously
- Call `cx.notify()` to trigger re-render

## Pattern Variations

### Window-Scoped Spawn (`cx.spawn_in`)

When you need window context (e.g., for focus, window operations):

```rust
self._task = Some(cx.spawn_in(window, async move |this, cx| {
    // Async work with window context
    this.update_in(cx, |this, window, cx| {
        // Access window for focus, dispatch, etc.
        window.dispatch_action(SomeAction, cx);
    }).ok();
}));
```

In the zed codebase, Used in `conversation_view.rs` for operations requiring window access [6](#3-5)  and `google.rs` for authentication UI .

### Background Spawn (`cx.background_spawn`)

For CPU-intensive or I/O work on background threads:

```rust
cx.background_spawn(async move {
    // Heavy computation or I/O
    let result = compute_heavy_thing().await;
    // Typically awaited by a foreground task to update UI
}).detach();
```

In the zed codebase, Used in `agent_panel.rs` for persistent storage operations.

## Task Lifecycle Management

Tasks must be managed to prevent cancellation:

| Method | Use Case | Example |
|--------|----------|---------|
| Store in field | Cancel when entity drops | `_save_task: Option<Task<()>>` [3](#3-2)  |
| `.detach()` | Run indefinitely | `task.detach()` [9](#3-8)  |
| `.detach_and_log_err(cx)` | Run indefinitely with error logging | `task.detach_and_log_err(cx)` [10](#3-9)  |
| Await | Chain operations | `task.await` [11](#3-10)  |

## Common Patterns from Codebase

### Throttled Operations

```rust
fn schedule_save(&mut self, cx: &mut Context<Self>) {
    self._save_task = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        this.update(cx, |this, cx| {
            // Perform save
            cx.notify();
        }).ok();
    }));
}
```

From `thread_view.rs` [12](#3-11) .

### Chained Async Operations

```rust
let contents_task = cx.spawn_in(window, async move |_this, cx| {
    let (contents, tracked_buffers) = contents.await?;
    // Process contents
    Ok(Some((contents, tracked_buffers)))
});

self.send_content(contents_task, false, window, cx);
```

In the zed codebase, From `thread_view.rs` [13](#3-12) .

### Authentication with Loading State

```rust
struct ConfigurationView {
    load_credentials_task: Option<Task<()>>,
}

impl ConfigurationView {
    fn new(state: Entity<State>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let load_credentials_task = Some(cx.spawn_in(window, {
            let state = state.clone();
            async move |this, cx| {
                let task = state.update(cx, |state, cx| state.authenticate(cx));
                let _ = task.await;
                this.update(cx, |this, cx| {
                    this.load_credentials_task = None;
                    cx.notify();
                }).log_err();
            }
        }));
        Self { load_credentials_task, .. }
    }
}
```

In the zed codebase, From `google.rs` [14](#3-13)  and `anthropic.rs` [15](#3-14) .

## Using GPUI Async

GPUI's native async system is built on **smol**, providing foreground and background executors for async operations.

### Executors

- **ForegroundExecutor**: Main thread executor for UI updates [16](#3-15) 
- **BackgroundExecutor**: Thread pool for CPU/I/O work [17](#3-16) 

### Accessing Executors

```rust
// In async context (AsyncApp or AsyncWindowContext)
cx.background_executor().timer(duration).await;
cx.foreground_executor().spawn(async move { ... });
```

### Timers

Use GPUI timers instead of `smol::Timer` for test compatibility [1](#3-0) :

```rust
cx.background_executor().timer(Duration::from_secs(1)).await;
```

## Using gpui_tokio for Tokio Integration

Some dependencies (like LiveKit SDK, certain HTTP clients, or WASM I/O) require Tokio's runtime. The `gpui_tokio` crate provides integration between GPUI's smol-based executors and Tokio.

### Initialization

Call `gpui_tokio::init(cx)` during app initialization to set up a Tokio runtime with 2 worker threads: [18](#3-17) 

```rust
fn init(cx: &mut App) {
    gpui_tokio::init(cx);
    // ... other initialization
}
```

### Spawning Tokio Tasks

Use `Tokio::spawn_result(cx, ...)` to run Tokio-based async work and integrate it with GPUI's task system: [19](#3-18) 

```rust
use gpui_tokio::Tokio;

// Spawn a Tokio task
let task = Tokio::spawn_result(cx, async move {
    // Tokio-based async work here
    Ok(result)
});
```

### When to Use gpui_tokio

Use `gpui_tokio` when:
- Working with Tokio-based SDKs (e.g., LiveKit) [20](#3-19) 
- Using HTTP clients that require Tokio context [21](#3-20) 
- Running WASM operations that use Tokio for I/O [22](#3-21) 

### Accessing Tokio Handle

For operations that need explicit Tokio context:

```rust
let _guard = Tokio::handle(cx).enter();
// Tokio-based code here
```

In the zed codebase, Used in `remote_server.rs` for HTTP client initialization [23](#3-22) .

## Notes

- All entity updates must happen on the foreground thread via `this.update(cx, ...)` [24](#3-23) 
- `AsyncApp` holds a `Weak<AppCell>` to avoid borrow issues across await points [25](#3-24) 
- Use `WeakEntity<T>` in async closures to prevent memory leaks [4](#3-3) 
- Never silently discard errors - use `.log_err()` or proper error handling [26](#3-25) 
- GPUI uses smol natively; only use `gpui_tokio` when Tokio is required by dependencies

Wiki pages you might want to explore:
- [Agent Panel and UI (zed-industries/zed)](https://deepwiki.com/zed-industries/zed/#8.1)
- [Language Model Integration (zed-industries/zed)](https://deepwiki.com/zed-industries/zed/#9)

Wiki pages you might want to explore:
- [Overview (zed-industries/zed)](https://deepwiki.com/zed-industries/zed/#1)
- [Collaboration Architecture (zed-industries/zed)](https://deepwiki.com/zed-industries/zed/#11.1)
