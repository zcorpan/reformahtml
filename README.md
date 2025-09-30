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
