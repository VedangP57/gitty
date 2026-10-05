#!/bin/sh
# Builds the made-up repository the demo recording runs in: a small weather CLI,
# fictional authors at example.com, fixed dates. Usage: make-repo.sh DIR
set -eu
dir=$1
rm -rf "$dir" && mkdir -p "$dir" && cd "$dir"
git init -q -b main
git config user.name "Mira Chen"
git config user.email mira@example.com

commit() { # commit AUTHOR EMAIL DATE MESSAGE
  git add -A
  GIT_AUTHOR_NAME=$1 GIT_AUTHOR_EMAIL=$2 GIT_AUTHOR_DATE=$3 \
  GIT_COMMITTER_NAME=$1 GIT_COMMITTER_EMAIL=$2 GIT_COMMITTER_DATE=$3 \
    git commit -q -m "$4"
}
mira() { commit "Mira Chen" mira@example.com "$1" "$2"; }
sam() { commit "Sam Okafor" sam@example.com "$1" "$2"; }
jo() { commit "Jo Rivera" jo@example.com "$1" "$2"; }

mkdir -p skylark tests
cat > README.md <<'EOF'
# skylark

A tiny weather forecast for your terminal.

    skylark Lisbon
EOF
cat > skylark/__init__.py <<'EOF'
__version__ = "0.1.0"
EOF
cat > skylark/forecast.py <<'EOF'
from dataclasses import dataclass


@dataclass
class Reading:
    city: str
    temp_c: float
    wind_kmh: float


def describe(r: Reading) -> str:
    return f"{r.city}: {r.temp_c:.0f}°C, wind {r.wind_kmh:.0f} km/h"
EOF
mira "2026-08-03T09:12:00+01:00" "Start skylark: a forecast for one city"

cat > skylark/cli.py <<'EOF'
import sys

from .forecast import Reading, describe
from .source import fetch


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: skylark CITY", file=sys.stderr)
        return 2
    print(describe(fetch(sys.argv[1])))
    return 0
EOF
cat > skylark/source.py <<'EOF'
from .forecast import Reading

SAMPLE = {
    "Lisbon": Reading("Lisbon", 24.0, 14.0),
    "Oslo": Reading("Oslo", 9.0, 22.0),
    "Nairobi": Reading("Nairobi", 21.0, 11.0),
}


def fetch(city: str) -> Reading:
    try:
        return SAMPLE[city]
    except KeyError:
        raise SystemExit(f"no forecast for {city}")
EOF
sam "2026-08-04T14:40:00+01:00" "Add the command line and a sample data source"

cat > tests/test_forecast.py <<'EOF'
from skylark.forecast import Reading, describe


def test_describe_rounds_values():
    assert describe(Reading("Oslo", 8.6, 21.7)) == "Oslo: 9°C, wind 22 km/h"
EOF
jo "2026-08-05T11:05:00+01:00" "Test how readings are described"

git switch -q -c feature/fahrenheit
cat > skylark/forecast.py <<'EOF'
from dataclasses import dataclass


@dataclass
class Reading:
    city: str
    temp_c: float
    wind_kmh: float


def to_fahrenheit(c: float) -> float:
    return c * 9 / 5 + 32


def describe(r: Reading, fahrenheit: bool = False) -> str:
    if fahrenheit:
        temp = f"{to_fahrenheit(r.temp_c):.0f}°F"
    else:
        temp = f"{r.temp_c:.0f}°C"
    return f"{r.city}: {temp}, wind {r.wind_kmh:.0f} km/h"
EOF
sam "2026-08-07T16:22:00+01:00" "Show temperatures in Fahrenheit on request"
cat >> tests/test_forecast.py <<'EOF'


def test_describe_in_fahrenheit():
    assert describe(Reading("Lisbon", 25, 10), fahrenheit=True) == "Lisbon: 77°F, wind 10 km/h"
EOF
sam "2026-08-08T10:03:00+01:00" "Test the Fahrenheit output"
git switch -q main
GIT_AUTHOR_DATE="2026-08-10T09:30:00+01:00" GIT_COMMITTER_DATE="2026-08-10T09:30:00+01:00" \
  git merge -q --no-ff feature/fahrenheit -m "Merge branch 'feature/fahrenheit'"
git branch -q -d feature/fahrenheit

cat > skylark/cli.py <<'EOF'
import argparse

from .forecast import describe
from .source import fetch


def main() -> int:
    parser = argparse.ArgumentParser(prog="skylark")
    parser.add_argument("city")
    parser.add_argument("-f", "--fahrenheit", action="store_true")
    args = parser.parse_args()
    print(describe(fetch(args.city), fahrenheit=args.fahrenheit))
    return 0
EOF
mira "2026-08-12T13:15:00+01:00" "Parse arguments with argparse and add --fahrenheit"
sed -i 's/__version__ = "0.1.0"/__version__ = "0.2.0"/' skylark/__init__.py
cat >> README.md <<'EOF'
    skylark --fahrenheit Oslo
EOF
jo "2026-08-14T08:50:00+01:00" "Release 0.2.0"
git tag v0.2.0

# Uncommitted work for the Changes tab: wind chill in the forecast, a new
# module, and a README line.
cat > skylark/forecast.py <<'EOF'
from dataclasses import dataclass


@dataclass
class Reading:
    city: str
    temp_c: float
    wind_kmh: float


def to_fahrenheit(c: float) -> float:
    return c * 9 / 5 + 32


def wind_chill(temp_c: float, wind_kmh: float) -> float:
    """The temperature it feels like, by the North American formula."""
    if temp_c > 10 or wind_kmh < 4.8:
        return temp_c
    v = wind_kmh ** 0.16
    return 13.12 + 0.6215 * temp_c - 11.37 * v + 0.3965 * temp_c * v


def describe(r: Reading, fahrenheit: bool = False) -> str:
    feels = wind_chill(r.temp_c, r.wind_kmh)
    if fahrenheit:
        temp = f"{to_fahrenheit(r.temp_c):.0f}°F (feels {to_fahrenheit(feels):.0f}°F)"
    else:
        temp = f"{r.temp_c:.0f}°C (feels {feels:.0f}°C)"
    return f"{r.city}: {temp}, wind {r.wind_kmh:.0f} km/h"
EOF
cat > skylark/units.py <<'EOF'
KMH_PER_MPH = 1.609344


def kmh_to_mph(kmh: float) -> float:
    return kmh / KMH_PER_MPH
EOF
cat >> README.md <<'EOF'

The forecast includes how cold the wind makes it feel.
EOF
