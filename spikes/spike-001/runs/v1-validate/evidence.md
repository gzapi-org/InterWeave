# v1-validate

## Run

```json
{
  "claude_version": "2.1.285",
  "command": [
    "claude",
    "plugin",
    "validate",
    "--strict",
    "<copy of plugin/interweave-spike>"
  ],
  "exit_by_case": {
    "as-committed": 0,
    "without-author": 1
  }
}
```

## `claude plugin validate --strict` output

```text
--- as-committed: exit 0
Validating plugin manifest: $SCRATCH

✔ Validation passed


--- without-author: exit 1
Validating plugin manifest: $SCRATCH

⚠ Found 1 warning:

  ❯ author: No author information provided. Consider adding author details for plugin attribution

✘ Validation failed (--strict treats warnings as errors)


```
