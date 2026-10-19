# Third-party notices

komposer reimplements [Composer](https://github.com/composer/composer) and the libraries below, and
bundles some of their files. They are all MIT licensed unless noted.

| Project | Copyright | What komposer contains |
|---|---|---|
| [composer/composer](https://github.com/composer/composer) | Nils Adermann, Jordi Boggiano | Behavior ported throughout; `res/composer-schema.json`, `res/composer-lock-schema.json`; `crates/autoload/res/ClassLoader.php` and `crates/autoload/res/LICENSE`, which are written into `vendor/composer/` as Composer writes them; the script API in `res/script-runtime.php` follows Composer's |
| [composer/semver](https://github.com/composer/semver) | 2015 Composer | `crates/semver` is a port |
| [composer/class-map-generator](https://github.com/composer/class-map-generator) | 2022 Composer | `crates/autoload` class map scanning is a port |
| [composer/metadata-minifier](https://github.com/composer/metadata-minifier) | 2021 Composer | metadata expansion in `crates/repository` is a port |
| [composer/spdx-licenses](https://github.com/composer/spdx-licenses) | 2015 Composer | `res/spdx-licenses.json`, `res/spdx-exceptions.json` and the license expression validation |
| [jsonrainbow/json-schema](https://github.com/jsonrainbow/json-schema) | 2016 | `src/json_schema.rs` is a port of its validator |
| [symfony/console](https://github.com/symfony/console) | Fabien Potencier | output formatting and exception rendering in `res/script-runtime.php` follow it |
| [php-src](https://github.com/php/php-src) (PHP License 3.01) | The PHP Group | the email regex in `src/json_schema.rs`, which is based on one by Michael Rushton (© 2009-10, http://squiloople.com/, "Feel free to use and redistribute this code. But please keep this copyright notice.") |

Composer's MIT license text is in [`crates/autoload/res/LICENSE`](crates/autoload/res/LICENSE); the
other MIT licenses read the same with their copyright lines above.
