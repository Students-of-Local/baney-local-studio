#!/usr/bin/env bash
set -euo pipefail

die() { echo "release-stable: $*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: scripts/release-stable.sh X.Y.Z (after promote.yml built the vX.Y.Z draft)"
version=$1
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z"
tag="v$version"
remote=${REMOTE:-origin}
url=$(git remote get-url "$remote") || die "no remote $remote; set REMOTE"
REPO=$(sed -E 's#^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)##; s#\.git$##' <<< "$url")
[[ $REPO =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "remote $remote ($url) is not a GitHub repository"

[ "$(gh api "repos/$REPO" -q .permissions.admin)" = true ] || die "only a repository admin moves stable"

ids=$(gh api "repos/$REPO/releases" --paginate -q ".[] | select(.draft and .tag_name == \"$tag\") | .id")
[ "$(wc -w <<< "$ids")" -eq 1 ] || die "expected one draft for $tag, found: ${ids:-none}; run promote.yml first"
commit=$(gh api "repos/$REPO/releases/$ids" -q .target_commitish)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || die "the $tag draft names $commit, not a commit"
gh api "repos/$REPO/releases/$ids/assets" --paginate -q '.[].name' | grep -qx 'local-studio-linux-x86_64.tar.gz' ||
  die "the $tag draft has no Linux build; promote.yml did not finish"

git fetch -q "$remote" "+refs/heads/stable:refs/remotes/$remote/stable" "+refs/tags/*:refs/tags/*"
git cat-file -e "$commit^{commit}" 2> /dev/null || git fetch -q "$remote" "$commit"
head=$(git rev-parse "refs/remotes/$remote/stable")

if [ "$head" != "$commit" ]; then
  git merge-base --is-ancestor "$head" "$commit" || die "stable's head $head is not an ancestor of $commit"
  open=$(gh api "repos/$REPO/issues?state=open&labels=regression&per_page=100" -q '[.[] | select(.pull_request | not)] | length')
  [ "$open" = 0 ] || die "$open open issue(s) labeled regression"
fi

existing=$(git ls-remote "$remote" "refs/tags/$tag" | cut -f1)
[ -z "$existing" ] || [ "$(git rev-parse "$existing^{commit}")" = "$commit" ] || die "$tag already exists on another commit"

echo "moving stable from ${head:0:12} to ${commit:0:12} and tagging $tag"
[ "$head" = "$commit" ] || git push "$remote" "$commit:refs/heads/stable"
[ -n "$existing" ] || git push "$remote" "$commit:refs/tags/$tag"

[ "$(git ls-remote "$remote" refs/heads/stable | cut -f1)" = "$commit" ] || die "stable did not move to $commit"
[ "$(git ls-remote "$remote" "refs/tags/$tag" | cut -f1)" = "$commit" ] || die "$tag is not on $commit"

gh workflow run stable.yml -R "$REPO" --ref stable -f version="$version"
echo "started stable.yml for $tag; follow it with: gh run list -R $REPO --workflow stable.yml --limit 1"
