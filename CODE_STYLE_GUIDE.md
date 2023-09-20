# Code Style Guide

## Introduction

Clippy cannot yet detect all suboptimal code. This guide supplements that.

This guide is incomplete. More may be added as more pull requests are going to be reviewed.

This is a guide, not a rule. Contributors may break them if they have a good reason to do so.

## Terminology

[owned]: #owned-type
[borrowed]: #borrowed-type
[copying]: #copying

### Owned type

Doesn't have a lifetime, neither implicit nor explicit.

*Examples:* `String`, `OsString`, `PathBuf`, `Vec<T>`, etc.

### Borrowed type

Has a lifetime, either implicit or explicit.

*Examples:* `&str`, `&OsStr`, `&Path`, `&[T]`, etc.

### Copying

The act of cloning or creating an owned data from another owned/borrowed data.

*Examples:*
* `owned_data.clone()`
* `borrowed_data.to_owned()`
* `OwnedType::from(borrowed_data)`
* `path.to_path_buf()`
* `str.to_string()`
* etc.

## Guides

### Naming convention

Follow [the Rust API guidelines](https://rust-lang.github.io/api-guidelines/naming.html).

### When to use [owned] parameter? When to use [borrowed] parameter?

This is a trade-off between API flexibility and performance.

If using an [owned] signature would reduce [copying], one should use an [owned] signature.

Otherwise, use a [borrowed] signature to widen the API surface.

**Example 1:** Preferring [owned] signature.

```rust
fn push_path(list: &mut Vec<PathBuf>, item: &Path) {
    list.push(item.to_path_buf());
}

push_path(my_list, my_path_buf);
push_path(my_list, my_path_ref.to_path_buf());
```

The above code is suboptimal because it forces the [copying] of `my_path_buf` even though the type of `my_path_buf` is already `PathBuf`.

Changing the signature of `item` to `PathBuf` would help remove `.to_path_buf()` inside the `push_back` function, eliminate the cloning of `my_path_buf` (the ownership of `my_path_buf` is transferred to `push_path`).

```rust
fn push_path(list: &mut Vec<PathBuf>, item: PathBuf) {
    list.push(item);
}

push_path(my_list, my_path_buf);
push_path(my_list, my_path_ref.to_path_buf());
```

It does force `my_path_ref` to be explicitly copied, but since `item` is not copied, the total number of copying remains the same for `my_path_ref`.

**Example 2:** Preferring [borrowed] signature.

```rust
fn show_path(path: PathBuf) {
    println!("The path is {path:?}");
}

show_path(my_path_buf);
show_path(my_path_ref.to_path_buf());
```

The above code is suboptimal because it forces the [copying] of `my_path_ref` even though a `&Path` is already compatible with the code inside the function.

Changing the signature of `path` to `&Path` would help remove `.to_path_buf()`, eliminating the unnecessary copying:

```rust
fn show_path(path: &Path) {
    println!("The path is {path:?}");
}

show_path(my_path_buf);
show_path(my_path_ref);
```

### Use the most encompassing type for function parameters

The goal is to allow the function to accept more types of parameters, reducing type conversion.

**Example 1:**

```rust
fn node_bin_dir(workspace: &PathBuf) -> PathBuf {
    workspace.join("node_modules").join(".bin")
}

let a = node_bin_dir(&my_path_buf);
let b = node_bin_dir(&my_path_ref.to_path_buf());
```

The above code is suboptimal because it forces the [copying] of `my_path_ref` only to be used as a reference.

Changing the signature of `workspace` to `&Path` would help remove `.to_path_buf()`, eliminating the unnecessary copying:

```rust
fn node_bin_dir(workspace: &Path) -> PathBuf {
    workspace.join("node_modules").join(".bin")
}

let a = node_bin_dir(&my_path_buf);
let b = node_bin_dir(my_path_ref);
```

### When or when not to log during tests? What to log? How to log?

The goal is to enable the programmer to quickly inspect the test subject should a test fails.

Logging is almost always necessary when the assertion is not `assert_eq!`. For example: `assert!`, `assert_ne!`, etc.

Logging is sometimes necessary when the assertion is `assert_eq!`.

If the values being compared with `assert_eq!` are simple scalar or single line strings, logging is almost never necessary. It is because `assert_eq!` should already show both values when assertion fails.

If the values being compared with `assert_eq!` are strings that may have many lines, they should be logged with `eprintln!` and `{}` format.

If the values being compared with `assert_eq!` have complex structures (such as a struct or an array), they should be logged with `dbg!`.

**Example 1:** Logging before assertion is necessary

```rust
let message = my_func().unwrap_err().to_string();
eprintln!("MESSAGE:\n{message}\n");
assert!(message.contains("expected segment"));
```

```rust
let output = execute_my_command();
let received = output.stdout.to_string_lossy(); // could have multiple lines
eprintln!("STDOUT:\n{received}\n");
assert_eq!(received, expected)
```

```rust
let hash_map = create_map(my_argument);
dbg!(&hash_map);
assert!(hash_map.contains_key("foo"));
assert!(hash_map.contains_key("bar"));
```

**Example 2:** Logging is unnecessary

```rust
let received = add(2, 3);
assert_eq!(received, 5);
```

If the assertion fails, the value of `received` will appear alongside the error message.

### Cloning an atomic counter

Prefer using `Arc::clone` or `Rc::clone` to vague `.clone()` or `Clone::clone`.

**Error resistance:** Explicitly specifying the cloned type would avoid accidentally cloning the wrong type. As seen below:

```rust
fn my_function(value: Arc<Vec<u8>>) {
    // ... do many things here
    let value_clone = value.clone(); // inexpensive clone
    tokio::task::spawn(async {
        // ... do stuff with value_clone
    })
}
```

The above function could easily refactored into the following code:

```rust
fn my_function(value: &Vec<u8>) {
    // ... do many things here
    let value_clone = value.clone(); // expensive clone, oops
    tokio::task::spawn(async {
        // ... do stuff with value_clone
    })
}
```

With an explicit `Arc::clone`, however, the performance characteristic will never be missed:

```rust
fn my_function(value: Arc<Vec<u8>>) {
    // ... do many things here
    let value_clone = Arc::clone(&value); // no compile error
    tokio::task::spawn(async {
        // ... do stuff with value_clone
    })
}
```

```rust
fn my_function(value: &Vec<u8>) {
    // ... do many things here
    let value_clone = Arc::clone(&value); // compile error
    tokio::task::spawn(async {
        // ... do stuff with value_clone
    })
}
```

The above code is still valid code, and the Rust compiler doesn't error, but it has a different performance characteristic now.

**Readability:** The generic `.clone()` or `Clone::clone` often implies an expensive operation (for example: cloning a `Vec`), but `Arc` and `Rc` are not as expensive as the generic `.clone()`. Explicitly mark the cloned type would aid in future refactoring.
