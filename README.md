# `memalert`

`memalert` displays (Linux) desktop notifications warning of low system memory.

## Installation

```
# TODO Publish to crates.io
cargo install --git https://github.com/andrewliebenow/memalert
```

## Usage

A desktop notification will be displayed if free system memory falls below a given threshold of total system memory.

```shell
❯ memalert --help
Displays a notification when free system memory falls below a threshold

Usage: memalert [OPTIONS]

Options:
  -d, --daemon
          Run as a daemon
  -m, --memory-free-percent <MEMORY_FREE_PERCENT>
          A notification will be displayed when the percentage of system memory that is free falls below this threshold. If not specified, a default value is used. Must be greater than or equal to 1, and less than or equal to 99
  -h, --help
          Print help
  -V, --version
          Print version
```

You will likely want to run `memalert` as a daemon (`memalert --daemon`).

## Demo

![`memalert` demo](memalert.png)

## License

MIT License, see <a href="LICENSE">LICENSE</a> file
