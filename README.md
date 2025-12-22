# m4rs

A Rust implementation of an M4 macro processor with support for recursive expansion, positional arguments, and built-in macros.

## Features

- **M4-like parsing** using [Pest](https://pest.rs/) PEG grammar (subset of M4 features)
- **Recursive macro expansion** with rescan behavior
- **Streaming expansion** via `ExpandingReader` implementing `std::io::Read`
- **Zero-copy parsing** with `Cow<str>` for efficient string handling

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

The library is organized into three main modules:

| Module | Description |
|--------|-------------|
| `ast` | Token types: `Token`, `MacroCall`, `Group` with `Cow<str>` for flexible ownership |
| `parser` | Pest-based parser that converts M4 source into an AST |
| `processor` | `MacroRegistry` for storing definitions and `Expander` for recursive expansion |


## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
