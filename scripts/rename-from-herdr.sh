#!/usr/bin/env bash
# scripts/rename-from-herdr.sh
# Automated, idempotent rename script from Herdr to Shepherd for Shepherd (herdr fork).
# Usage:
#   ./scripts/rename-from-herdr.sh           # Perform rename
#   ./scripts/rename-from-herdr.sh --dry-run # Show files and replacement counts without writing
#   ./scripts/rename-from-herdr.sh --check   # Check if any unexcluded occurrences of herdr remain (exit 0 if clean, 1 if found)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MODE="run"
if [ "${1:-}" = "--dry-run" ]; then
    MODE="dry-run"
elif [ "${1:-}" = "--check" ]; then
    MODE="check"
elif [ "${1:-}" = "--apply" ] || [ "${1:-}" = "--run" ] || [ -z "${1:-}" ]; then
    MODE="run"
else
    echo "Unknown option: $1" >&2
    echo "Usage: $0 [--dry-run | --check | --apply]" >&2
    exit 2
fi

export RENAME_MODE="$MODE"
export REPO_ROOT

perl -e '
use strict;
use warnings;
use File::Copy qw(move);

my $mode = $ENV{RENAME_MODE} || "run";
my $repo_root = $ENV{REPO_ROOT} || ".";

# Path exclusions: files or directories that must never be renamed or modified
my %exact_path_exclusions = map { $_ => 1 } qw(
    LICENSE
    NOTICE
    CHANGELOG.md
    Cargo.lock
    docs/next/CHANGELOG.md
    tests/fixtures/endpoint-snapshot-v1.json
    tests/fixtures/session/legacy-pre-tabs-v2.json
    distribution/latest.json
    distribution/preview.json
    skills/herdr/SKILL.md
    scripts/rename-from-herdr.sh
    scripts/upstream-sync.sh
    docs/UPSTREAM-SYNC.md
    README.md
    README.zh-CN.md
);

sub is_path_excluded {
    my ($path) = @_;
    return 1 if $exact_path_exclusions{$path};
    return 1 if $path =~ m{^vendor/};
    return 1 if $path =~ m{^target/};
    return 1 if $path =~ m{^docs/versions/};
    return 1 if $path =~ m{^docs/preview/};
    return 1 if $path =~ m{^SHEPHERD};
    return 0;
}

sub is_rename_path_excluded {
    my ($path) = @_;
    return 1 if is_path_excluded($path);
    return 1 if $path =~ m{^\.agents/};
    return 0;
}

# Line pattern exclusions: lines matching these patterns are preserved verbatim
sub is_line_excluded {
    my ($line, $file) = @_;
    return 1 if $line =~ m{github\.com/herdrdev/herdr};
    return 1 if $line =~ m{https?://(www\.)?herdr\.dev};
    return 1 if $line =~ m{\bherdrdev\b};
    return 1 if $line =~ m{ogulcancelik/herdr-plugin-examples};
    return 1 if $line =~ m{skills/herdr/SKILL\.md};
    return 1 if $line =~ m{fork (de|of) herdr|[Cc]r[ée]dito.*herdr|Credit to herdr};
    return 1 if $line =~ m{SPDX-License-Identifier.*Apache-2\.0};
    return 1 if $line =~ m{herdr_fallback_config_path|falling back to herdr config|herdr_name|herdr_cfg|config_load_falls_back_to_herdr_config|points to shepherd, not herdr|fallback herdr|wire contract v1 compatibility};
    return 1 if defined $file && $file =~ m{shepherd-api\.schema\.json$} && ($line =~ m{^\s*"herdr",?\s*$} || $line =~ m{^\s*"default":\s*"herdr"});
    return 0;
}

my @tracked_files = `git ls-files`;
chomp @tracked_files;

if ($mode eq "check") {
    my $found_unexcluded = 0;
    for my $file (@tracked_files) {
        next if is_path_excluded($file);
        next unless -f $file;
        open my $fh, "<", $file or next;
        my $ln = 0;
        while (my $line = <$fh>) {
            $ln++;
            next if is_line_excluded($line, $file);
            if ($line =~ /(HERDR|Herdr|herdr)/) {
                print "$file:$ln: $line";
                $found_unexcluded++;
            }
        }
        close $fh;
    }
    if ($found_unexcluded > 0) {
        print STDERR "\n[FAIL] Found $found_unexcluded unexcluded occurrences of herdr/Herdr/HERDR.\n";
        exit 1;
    } else {
        print "[OK] No unexcluded occurrences of herdr/Herdr/HERDR found.\n";
        exit 0;
    }
}

# In dry-run or run mode:
my $total_files_changed = 0;
my $total_replacements = 0;
my %rep_by_kind = ( HERDR => 0, Herdr => 0, herdr => 0 );

for my $file (@tracked_files) {
    next if is_path_excluded($file);
    next unless -f $file;

    open my $fh, "<", $file or die "Cannot open $file: $!";
    my @lines;
    my $file_modified = 0;
    my ($c_herdr, $c_Herdr, $c_HERDR) = (0, 0, 0);

    while (my $line = <$fh>) {
        if (is_line_excluded($line, $file)) {
            push @lines, $line;
            next;
        }

        my $orig = $line;

        # Protect specific compounds from turning into double shepherd
        $line =~ s/herdr_shepherd/shepherd/g;
        $line =~ s/HerdrShepherd/Shepherd/g;

        # Case-sensitive replacements
        my $n_HERDR = ($line =~ s/HERDR/SHEPHERD/g) || 0;
        my $n_Herdr = ($line =~ s/Herdr/Shepherd/g) || 0;
        my $n_herdr = ($line =~ s/herdr/shepherd/g) || 0;

        if ($n_HERDR || $n_Herdr || $n_herdr || $line ne $orig) {
            $file_modified = 1;
            $c_HERDR += $n_HERDR;
            $c_Herdr += $n_Herdr;
            $c_herdr += $n_herdr;
        }
        push @lines, $line;
    }
    close $fh;

    if ($file_modified) {
        my $file_total = $c_HERDR + $c_Herdr + $c_herdr;
        $total_files_changed++;
        $total_replacements += $file_total;
        $rep_by_kind{HERDR} += $c_HERDR;
        $rep_by_kind{Herdr} += $c_Herdr;
        $rep_by_kind{herdr} += $c_herdr;

        if ($mode eq "dry-run") {
            print "$file: $file_total replacements ($c_HERDR HERDR, $c_Herdr Herdr, $c_herdr herdr)\n";
        } elsif ($mode eq "run") {
            open my $out, ">", $file or die "Cannot write $file: $!";
            print $out @lines;
            close $out;
        }
    }
}

print "\nContent replacement summary:\n";
print "  Files modified: $total_files_changed\n";
print "  Total replacements: $total_replacements (HERDR: $rep_by_kind{HERDR}, Herdr: $rep_by_kind{Herdr}, herdr: $rep_by_kind{herdr})\n";

# File renaming phase
my @renames;
for my $file (@tracked_files) {
    next if is_rename_path_excluded($file);
    if ($file =~ /herdr/i) {
        my $new_name = $file;
        $new_name =~ s/HERDR/SHEPHERD/g;
        $new_name =~ s/Herdr/Shepherd/g;
        $new_name =~ s/herdr/shepherd/g;
        if ($new_name ne $file) {
            push @renames, [$file, $new_name];
        }
    }
}

print "\nFile renames:\n";
for my $pair (@renames) {
    my ($old, $new) = @$pair;
    if ($mode eq "dry-run") {
        print "  [dry-run] $old -> $new\n";
    } elsif ($mode eq "run") {
        print "  $old -> $new\n";
        system("git", "mv", "-f", $old, $new) == 0 or move($old, $new) or warn "Failed to rename $old to $new: $!";
    }
}
'

if [ "$MODE" = "run" ]; then
    # Specific adjustment: crates/shepherd-core/Cargo.toml binary rename to shepherd-core
    if [ -f "crates/shepherd-core/Cargo.toml" ]; then
        perl -i -pe 's/name = "shepherd"/name = "shepherd-core"/ if /^name = "shepherd"/' crates/shepherd-core/Cargo.toml
    fi
    echo "Rename step completed."
fi
