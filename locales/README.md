# GUI translations

One folder per language (`ko`, `ja`, `zh`), and in each one JSON file per GUI
area with the same name. Each file is a flat map from the exact English source
text to its translation:

```json
{ "Mount": "마운트" }
```

- English is the source and the fallback; `ko` (해요체), `ja` and `zh` (Simplified) are required for every key.
- Every area must exist in all three folders with the same keys.
- Areas are compiled in via `TABLES` in `src/gui/i18n/mod.rs`; a new area must be registered there.
- `cargo test every_wrapped_gui_string_is_translated` lists any `tr()`/`trf()` string without a translation;
  `tables_parse_and_every_entry_has_all_three_languages` lists keys missing from a language.
