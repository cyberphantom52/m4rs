# m4rs

A Rust implementation of an M4 macro processor following GNU m4's expansion rules.

## Features

- **GNU m4 semantics**: a streaming expander with a pushback input stack, so expansions are rescanned, arguments are expanded as they are collected, and macro names are recognized anywhere in the text (after `:` or `-`, inside `0x...-NAME`)
- **Builtins**: `define`, `undefine`, `ifdef`, `ifelse` (with chained conditions), `eval` (32-bit integer arithmetic with C operators and `**`), `incr`, `decr`, `len`, `shift` and `dnl`, plus `$0`-`$N`, `$#`, `$*` and `$@`
- **Streaming expansion** via `ExpandingReader` implementing `std::io::Read`
- **Bounded**: GNU m4's nesting limit of 1024 pending calls, kept on the heap rather than the call stack, and a rescan-depth limit that turns infinite recursion into an error

Quoting uses `` ` `` and `'`; comments start with `#` and pass through unexpanded. `changequote`, `changecom`, `divert`, `include` and the other GNU builtins are not implemented.

Unlike GNU m4, a malformed number in `eval`, `incr` or `decr` (including division by zero) is an error rather than a warning with empty output.

## Usage

### Basic Macro Expansion

```rust
use m4rs::processor::{MacroRegistry, Expander};

fn main() {
    // Create a macro registry
    let mut registry = MacroRegistry::new();

    // Load macro definitions from source
    registry.load("define(`greet', `Hello, $1!')").unwrap();

    // Create an expander and expand text
    let mut expander = Expander::new(registry);
    let result = expander.expand("greet(`World')").unwrap();

    println!("{}", result); // Output: Hello, World!
}
```

### Loading Macros from Files

```rust
use m4rs::processor::MacroRegistry;

let mut registry = MacroRegistry::new();
registry.load_file("macros.m4").expect("Failed to load macros");
```

### Streaming Expansion with `ExpandingReader`

```rust
use std::fs::File;
use std::io::Read;
use m4rs::processor::{ExpandingReader, MacroRegistry};

let mut registry = MacroRegistry::new();
registry.load_file("macros.m4").expect("Failed to load macros");

let input = File::open("input.m4").expect("Failed to open input");
let mut reader = ExpandingReader::new(input, registry);

let mut output = String::new();
reader.read_to_string(&mut output).expect("Failed to expand");
```



## Architecture

| Module | Description |
|--------|-------------|
| `processor` | `MacroRegistry` for definitions, `Expander` and `ExpandingReader` for expansion |
| `error` | `M4Error` |

Expansion output has been checked against GNU m4 on the policy sources of AOSP's `system/sepolicy`, with identical results.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
