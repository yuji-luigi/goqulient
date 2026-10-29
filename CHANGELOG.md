# Changelog

## [0.1.0](https://github.com/yuji-luigi/goqulient/compare/v0.1.0...v0.1.0) (2026-10-29)


### ⚠ BREAKING CHANGES

* **vscode:** settings, commands and saved passwords move from goku.* to goqulient.* (no released version used the old ids).

### Features

* **engine:** ER layout port with the er/layout method ([0a2ab7a](https://github.com/yuji-luigi/goqulient/commit/0a2ab7a44bd4160f1b480083446478d98ed022c5))
* **engine:** MongoDB driver (connect, catalog, browse, inference) ([50bf919](https://github.com/yuji-luigi/goqulient/commit/50bf91930001a117833b8de644433776bcdb07ab))
* **engine:** MySQL/MariaDB driver ([220530e](https://github.com/yuji-luigi/goqulient/commit/220530e051a07b4cf539066d0f7bc0ed2e3fe172))
* **engine:** PostgreSQL driver ([3cde310](https://github.com/yuji-luigi/goqulient/commit/3cde3106a4b876266801021f9810a793b04c774c))
* **engine:** Rust engine skeleton with the stdio JSON-RPC server ([c7750cc](https://github.com/yuji-luigi/goqulient/commit/c7750cca427283adb34322689c19dec7ed88ad2a))
* **engine:** session contract, schema model and the connection/catalog methods ([0b5343a](https://github.com/yuji-luigi/goqulient/commit/0b5343a30b7a940f0f1f9cb593c9d2aa35d4d432))
* **vscode:** connections from settings and .env, SecretStorage, Workspace Trust ([f56bf47](https://github.com/yuji-luigi/goqulient/commit/f56bf4726b081d94356ac78af478109bd70b55ad))
* **vscode:** Docker-style POSTGRES_* and MYSQL_ROOT_PASSWORD env variables ([21c81f7](https://github.com/yuji-luigi/goqulient/commit/21c81f7b7457f28c04a803698147461cb618606d))
* **vscode:** extension skeleton that runs the Rust engine ([2072329](https://github.com/yuji-luigi/goqulient/commit/207232933d914532e562b81298393fb88c1e38cf))
* **vscode:** show only the profile's database, and name the connection in panel titles ([939b70c](https://github.com/yuji-luigi/goqulient/commit/939b70c4923c49469dd979f400da46a377ef3908))
* **vscode:** sidebar tree with lazy connect, table and ER wiring (VSX-11) ([bb4df0b](https://github.com/yuji-luigi/goqulient/commit/bb4df0b808a2e61edbe7a9d9a2407d7f5d9bbd64))
* **vscode:** table viewer and ER diagram webviews (VSX-12, VSX-13) ([1acd24d](https://github.com/yuji-luigi/goqulient/commit/1acd24d2b49a362606b2a6990e9b745b347ef092))


### Bug Fixes

* **ci:** package every target with one script that creates the output folder ([52b0c43](https://github.com/yuji-luigi/goqulient/commit/52b0c432ce65e6da734213c7e8bfd27e55412165))


### Code Refactoring

* **vscode:** rename the extension to GoQulient ([81d2aa6](https://github.com/yuji-luigi/goqulient/commit/81d2aa6c4408d2f07a792707304614d20237ba43))
