"""Marmot export."""
DEFAULT_DELIMITER = ";"


def write_row(values, delimiter=DEFAULT_DELIMITER):
    return delimiter.join(str(v) for v in values)
