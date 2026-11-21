# CSV export

`marmot export` writes CSV with a semicolon as the default delimiter. Users in comma-decimal locales
(for example German or Spanish spreadsheets) got broken columns when the delimiter was a comma, because
the decimal comma split numbers in two. `--delimiter ,` still works.
