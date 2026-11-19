# Semble grammar sources

The parsers below match the [source manifest of Python `semble-grammars` 0.1.2][manifest], used by
Semble 0.7.3. They compile into the Rust binaries; no Python, grammar generator, or runtime download is needed.
A host binary may link a different Vue revision alongside this one.

| Language | Upstream source pinned by Python | Rust build input | ABI | License |
| --- | --- | --- | --- | --- |
| Haskell | [`tree-sitter/tree-sitter-haskell` at `0975ef72fc3c47b530309ca93937d7d143523628`][haskell] | `tree-sitter-haskell` 0.23.1. Its parser, scanner, and all C headers are byte-for-byte equal to the Python pin, even though the crate's Git revision differs. | 14 | MIT |
| Lua | [`tree-sitter-grammars/tree-sitter-lua` at `10fe0054734eec83049514ea2e718b2a56acd0c9`][lua] | `tree-sitter-lua` 0.5.0, published from the same commit; its C sources and headers are identical. | 15 | MIT |
| Vue | [`tree-sitter-grammars/tree-sitter-vue` at `ce8011a414fdf8091f4e4071752efc376f4afb08`][vue] | `vue/src`: published `parser.c`, `scanner.c`, `tag.h`, and `tree_sitter/{alloc,array,parser}.h`, copied unchanged. GitHub source archive SHA-256: `e62913c5709ed1e68616bb85aedb2641a0c35c6b6709986ab71487d54b16ad0d`. The C build prefixes its entry points so this revision can link alongside the folded-context parser. | 15 | [MIT](vue/LICENSE) |

[manifest]: https://github.com/MinishLab/semble-grammars/blob/6420821e53dd227ece0bea3a41e1577bb34d86c7/src/semble_grammars/grammars/sources.json
[haskell]: https://github.com/tree-sitter/tree-sitter-haskell/tree/0975ef72fc3c47b530309ca93937d7d143523628
[lua]: https://github.com/tree-sitter-grammars/tree-sitter-lua/tree/10fe0054734eec83049514ea2e718b2a56acd0c9
[vue]: https://github.com/tree-sitter-grammars/tree-sitter-vue/tree/ce8011a414fdf8091f4e4071752efc376f4afb08
