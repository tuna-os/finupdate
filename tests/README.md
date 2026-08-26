# Finupdate Test Suite

## Overview

- **GUI Tests (`tests/gui/test_features.py`)**: Driven via `harness.py` under Broadway / GTK4.
- **Rust Core**: Unit tests within `finupdate-core` and UI sub-crates.

## Running GUI Tests

```bash
python3 tests/gui/test_features.py
```

## Build Dependencies

Running the Rust test suite requires Cargo and a C compiler / linker (`cc` or `gcc`) installed in the build environment.
