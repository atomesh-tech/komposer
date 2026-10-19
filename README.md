# komposer

[Composer](https://getcomposer.org), the PHP dependency manager, rewritten in Rust as a single
binary. Installing, updating and generating the autoloader do not need PHP; scripts that run PHP use
the PHP that is installed.

It reads and writes the same files as Composer 2.10: `composer.json`, `composer.lock`, `vendor/`,
`vendor/composer/*` and `vendor/bin/*`. Its output is byte-identical to Composer's for the same
input, and it uses Composer's cache and `auth.json` locations. You and your teammates can switch
between `komposer` and `composer` on the same project.

## Install

```sh
cargo install --path .
```

## Use

```sh
komposer install                          # vendor/ from composer.lock
komposer require monolog/monolog
komposer create-project vendor/skeleton my-app
```

## Commands

`install`, `update`, `require`, `remove`, `dump-autoload`, `create-project`, `show`,
`outdated`, `depends` (`why`), `prohibits` (`why-not`), `run-script`, `exec`,
`validate`, `audit`, `check-platform-reqs`, `search`, `bump`, `reinstall`, `status`,
`config`, `init`, `global`, `licenses`, `fund`, `suggests`, `browse`, `clear-cache`,
`diagnose`, and scripts from composer.json run as commands (`komposer test`). Options
match Composer's.

## Differences from Composer

- **Composer plugins are not run.** They are PHP code that hooks into Composer itself. Plugin
  packages are still installed, and komposer warns about them.
- **Scripts run like in Composer, PHP ones with the PHP that is installed.** Shell commands, `@php`,
  `@composer`, `@putenv`, references to other scripts, PHP callbacks (`Vendor\Class::method`) and
  Command classes all run, for every event Composer has, including the package events. PHP callbacks
  get komposer's own implementation of the Composer API they use (`Composer\Script\Event`,
  `getComposer()`, its config, IO, packages and repositories); the few parts of Composer it does not
  provide fail with an explanation. Without PHP, `@php` scripts and PHP callbacks are skipped and
  listed.
- **Without PHP, `php`, `ext-*` and `lib-*` requirements can't be checked**, so komposer
  ignores them and says so. To resolve dependencies for a specific PHP version, set
  `config.platform.php` in composer.json; komposer then checks against that version.
- **hg, svn, fossil and perforce sources are not supported.** git, path, zip and tar sources
  are.
- **Errors are printed as plain messages**, without the frame Composer's console prints around an
  exception (the `In File.php line N:` box and the command's usage line).

## Development

The workspace is split the same way Composer's source is:

| crate | Composer part |
|---|---|
| `crates/semver` | composer/semver |
| `crates/package` | packages, loaders, dumpers, config, JSON |
| `crates/solver` | DependencyResolver, RepositorySet, VersionSelector |
| `crates/repository` | repositories, HTTP, cache, git, platform |
| `crates/autoload` | AutoloadGenerator, class-map-generator |
| `crates/installer` | downloaders, installers, installed.json/php |
| `src/` | CLI, Installer, Locker, scripts, commands |

```sh
cargo test --workspace
```
