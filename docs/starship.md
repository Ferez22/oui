# Starship integration

Every shell oUI starts gets `OUI_PROFILE` (profile name) and `OUI_PROFILE_COLOR`
(its colour). Tabs in your normal environment get `OUI_PROFILE=system`.
`~/.config/starship.toml` is shared into profiles by default, so one config
covers all of them.

## Show the profile

Add to `~/.config/starship.toml`:

```toml
[custom.oui]
description = "oUI profile"
when = 'test -n "$OUI_PROFILE" && test "$OUI_PROFILE" != system'
command = 'echo $OUI_PROFILE'
symbol = '● '
style = 'bold blue'
format = '[$symbol$output]($style) '
```

`when` runs in your shell; the module is hidden when it exits non-zero, so it
only shows inside oUI profile tabs. To place it explicitly in your top-level
`format`, use `${custom.oui}` (the `${…}` form is required because of the dot).

## Use the profile colour

Starship styles are static, so the colour can't be read from
`OUI_PROFILE_COLOR` directly. Add one module per profile instead, with the
colour you gave it (`oui profile list` shows them):

```toml
[custom.oui_work]
description = "oUI profile: work"
when = 'test "$OUI_PROFILE" = work'
command = 'echo work'
symbol = '● '
style = 'bold green'
format = '[$symbol$output]($style) '

[custom.oui_personal]
description = "oUI profile: personal"
when = 'test "$OUI_PROFILE" = personal'
command = 'echo personal'
symbol = '● '
style = 'bold magenta'
format = '[$symbol$output]($style) '
```

## Lighter alternative

The built-in `env_var` module needs no shell command and hides itself when the
variable is unset (it will show `system` in system tabs):

```toml
[env_var.OUI_PROFILE]
format = '[● $env_value]($style) '
style = 'bold blue'
```

Options used here are from Starship's
[custom commands](https://starship.rs/config/#custom-commands) and
[environment variable](https://starship.rs/config/#environment-variable) docs.
