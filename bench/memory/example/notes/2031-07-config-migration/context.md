# Config migration

On first start Marmot looks for the old `marmot.ini` and converts it to `config.toml`. The old file is
renamed to `marmot.ini.bak`; it is never deleted. If `config.toml` already exists the migration is
skipped, so a hand-edited file is never overwritten.
