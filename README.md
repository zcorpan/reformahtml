# reformahtml

Command line tool to format HTML or Bikeshed-flavored markdown to no-line-breaks but keep original indentation.

## Install

```bash
$ cargo install reformahtml
```

## Usage

```bash
$ reformahtml [--markdown | --no-markdown] <INPUT> [OUTPUT]
```

* With a single path, the input file is overwritten.
* With two paths, the second is written as the output.
* No stdout output.

If an element should not be reformatted, add the `data-noreformat` attribute.

## Running Tests

To run the regression tests:

```
cargo test
```

## Adding or Updating Tests

Regression tests use fixture files in `tests/fixtures/inputs` (inputs) and `tests/fixtures/expected` (expected outputs). Processes `.bs` (with Markdown enabled) and `.html` (with Markdown disabled) files.

- **Add a new test**: Place a new input file (e.g., `my_test.bs` or `my_test.html`) in `tests/fixtures/inputs`. Run `UPDATE_EXPECTED=1 cargo test` to generate the corresponding expected file in `tests/fixtures/expected`.

- **Update an existing test**: Modify the input file or code, then run `UPDATE_EXPECTED=1 cargo test` to regenerate the expected file.
