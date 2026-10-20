#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
cd "$SCRIPT_DIR/.."

version="${1:-}"
if [[ -z $version ]]; then
  echo "USAGE: $0 version" >&2
  exit 1
fi

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "version must match X.Y.Z without 'v' prefix, got: ${version}" >&2
  exit 1
fi
tag="v${version}"

if [[ "$(git symbolic-ref --short HEAD)" != "main" ]]; then
  echo "must be on main branch" >&2
  exit 1
fi

uncommitted_changes=$(git diff --compact-summary)
if [[ -n $uncommitted_changes ]]; then
  echo -e "There are uncommitted changes, exiting:\n${uncommitted_changes}" >&2
  exit 1
fi
git pull git@github.com:Mic92/formatelf main
unpushed_commits=$(git log --format=oneline origin/main..main)
if [[ -n $unpushed_commits ]]; then
  echo -e "\nThere are unpushed changes, exiting:\n$unpushed_commits" >&2
  exit 1
fi
if git tag -l | grep -q "^${tag}\$"; then
  echo "Tag ${tag} already exists, exiting" >&2
  exit 1
fi

sed -i -e "0,/^version = \".*\"\$/s//version = \"${version}\"/" Cargo.toml
nix develop -c cargo update --workspace --offline
git add Cargo.toml Cargo.lock
git branch -D "release-${version}" || true
git checkout -b "release-${version}"
git commit -m "release ${version}"
git push origin "release-${version}"
pr_url=$(gh pr create \
  --base main \
  --head "release-${version}" \
  --title "Release ${version}" \
  --body "Release ${version} of formatelf")
gh pr merge "${pr_url##*/}" --auto --merge --delete-branch
git checkout main

until gh pr view "release-${version}" --json state --jq .state | grep -q MERGED; do
  echo "Waiting for PR to be merged..."
  sleep 5
done
git pull git@github.com:Mic92/formatelf main
gh release create "${tag}" --draft --title "formatelf ${version}" --notes ""
