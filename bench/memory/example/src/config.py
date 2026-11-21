"""Marmot config loading and one-time migration."""
import os


def migrate(directory):
    ini, toml = os.path.join(directory, "marmot.ini"), os.path.join(directory, "config.toml")
    if os.path.exists(toml) or not os.path.exists(ini):
        return
    # converting INI keys to TOML omitted in this example
    os.rename(ini, ini + ".bak")
