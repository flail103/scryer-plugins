#!/usr/bin/env bash

set -euo pipefail

# Prints one `plugin_id<TAB>version<TAB>commit` row per plugin component a
# release publishes.
#
# A trigger created by `xtask release-publish-tags` lists its plugin
# components in its signed tag body as a `components <n>` marker followed by
# one `component <tag> <commit>` line each, so one trigger can publish several
# lines of one plugin, each built from its own commit (own lockfile, own SDK)
# by the trigger's xtask. With a trigger tag given, that body is the only
# source: a trigger without the marker, or a lightweight one, is refused.
# Without a trigger tag (catalog-only and manual runs) the rows are the plugin
# tags pointing at the release commit plus the components listed by release
# triggers created there.
#
# Every row is checked against the repository: the component tag must be
# annotated, point at the listed commit, and that commit must be an ancestor
# of the release commit. With VERIFY=true the trigger tag and every component
# tag must also be signature-verified by GitHub (gh api).
#
# Automated releases. SCRYER_AUTOMATED_RELEASE_BOT_EMAIL names the refresh
# bot's committer email. GitHub verifies a tag only when its signing key
# belongs to the account owning the tagger email, so with VERIFY=true a
# verified tag whose tagger is that email was signed by the bot. Such a tag
# may only release AUTOMATED_RELEASE_PLUGIN_IDS, and only as a stable patch
# bump on the latest release line of that plugin; a bot-signed trigger may
# list nothing else. Human-signed tags are not affected.
AUTOMATED_RELEASE_PLUGIN_IDS=(cardigann-engine)

release_plugin_components() {
  local trigger_tag="${1:-}"
  local release_sha="${2:?release commit is required}"
  local repository="${3:?repository is required}"
  local verify="${4:-false}"
  local prefix="${SCRYER_OFFICIAL_PLUGIN_RELEASE_TAG_PREFIX:-plugins-v3}"

  if [[ ! "${release_sha}" =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Release commit must be a full sha: %s\n' "${release_sha}" >&2
    return 1
  fi

  local -a rows=()
  local tag sha
  local bot_email="${SCRYER_AUTOMATED_RELEASE_BOT_EMAIL:-}"
  local trigger_by_bot=false
  if [[ -n "${trigger_tag}" ]]; then
    if [[ "$(git cat-file -t "refs/tags/${trigger_tag}" 2>/dev/null || true)" != tag ]]; then
      printf 'Release trigger %s is not an annotated tag\n' "${trigger_tag}" >&2
      return 1
    fi
    if [[ "${verify}" == true ]]; then
      local trigger_tagger
      trigger_tagger="$(_release_plugin_components_verify_tag "${repository}" "${trigger_tag}")" || return 1
      if _release_plugin_components_is_bot "${trigger_tagger}" "${bot_email}"; then
        trigger_by_bot=true
      fi
    fi
    local declared
    declared="$(_release_plugin_components_from_trigger "${trigger_tag}")" || return 1
    if [[ -z "${declared}" ]]; then
      printf 'Release trigger %s carries no components marker; refusing to guess its components\n' "${trigger_tag}" >&2
      return 1
    fi
    if [[ "${declared}" != none ]]; then
      while IFS= read -r line; do
        [[ -n "${line}" ]] || continue
        rows+=("${line}")
      done <<<"${declared}"
    fi
  else
    local pointing
    pointing="$(git tag --points-at "${release_sha}")"
    while IFS= read -r tag; do
      [[ -n "${tag}" ]] || continue
      rows+=("${tag}"$'\t'"${release_sha}")
    done < <(printf '%s\n' "${pointing}" | grep -E "^${prefix}/[^/]+/v.+$" | sort || true)
    local trigger declared
    while IFS= read -r trigger; do
      [[ -n "${trigger}" ]] || continue
      declared="$(_release_plugin_components_from_trigger "${trigger}")" || return 1
      [[ -n "${declared}" && "${declared}" != none ]] || continue
      while IFS= read -r line; do
        [[ -n "${line}" ]] || continue
        rows+=("${line}")
      done <<<"${declared}"
    done < <(printf '%s\n' "${pointing}" | grep -E "^${prefix}/release/" | grep -Ev "^${prefix}/release/catalog-" | sort || true)
  fi

  local row spec plugin_id version tag_object resolved
  local seen=$'\n'
  local -a output=()
  for row in ${rows[@]+"${rows[@]}"}; do
    tag="${row%%$'\t'*}"
    sha="${row#*$'\t'}"
    if [[ "${seen}" == *$'\n'"${tag}"$'\t'"${sha}"$'\n'* ]]; then
      # The same component listed by both the release commit's tags and a
      # trigger body there.
      continue
    fi
    if [[ "${seen}" == *$'\n'"${tag}"$'\t'* ]]; then
      printf 'Component %s is listed at more than one commit\n' "${tag}" >&2
      return 1
    fi
    seen+="${tag}"$'\t'"${sha}"$'\n'
    if [[ "${tag}" != "${prefix}"/*/v* ]]; then
      printf 'Component tag is not a plugin release tag: %s\n' "${tag}" >&2
      return 1
    fi
    spec="${tag#"${prefix}"/}"
    plugin_id="${spec%%/*}"
    version="${spec#*/v}"
    if [[ ! "${plugin_id}" =~ ^[a-z0-9][a-z0-9-]*$ || -z "${version}" || "${prefix}/${plugin_id}/v${version}" != "${tag}" ]]; then
      printf 'Component tag carries an unsafe id or version: %s\n' "${tag}" >&2
      return 1
    fi
    if [[ ! "${sha}" =~ ^[0-9a-f]{40}$ ]]; then
      printf 'Component %s lists an invalid commit: %s\n' "${tag}" "${sha}" >&2
      return 1
    fi
    if ! tag_object="$(git rev-parse --verify --quiet "refs/tags/${tag}^{tag}")"; then
      printf 'Component %s is not an annotated tag\n' "${tag}" >&2
      return 1
    fi
    resolved="$(git rev-parse --verify --quiet "refs/tags/${tag}^{commit}")"
    if [[ "${resolved}" != "${sha}" ]]; then
      printf 'Component %s points at %s, not the listed %s\n' "${tag}" "${resolved}" "${sha}" >&2
      return 1
    fi
    if ! git merge-base --is-ancestor "${sha}" "${release_sha}"; then
      printf 'Component %s commit %s is not an ancestor of the release commit %s\n' "${tag}" "${sha}" "${release_sha}" >&2
      return 1
    fi
    if [[ "${verify}" == true ]]; then
      local tagger
      tagger="$(_release_plugin_components_verify_tag "${repository}" "${tag}" "${tag_object}")" || return 1
      if [[ "${trigger_by_bot}" == true ]] || _release_plugin_components_is_bot "${tagger}" "${bot_email}"; then
        _release_plugin_components_automated_release_allowed "${plugin_id}" "${version}" "${tag}" "${prefix}" || return 1
      fi
    fi
    output+=("${plugin_id}"$'\t'"${version}"$'\t'"${sha}")
  done
  # Nothing is printed until every row has passed, so a consumer never sees
  # a partial plan.
  for row in ${output[@]+"${output[@]}"}; do
    printf '%s\n' "${row}"
  done
}

# Prints the `tag<TAB>commit` rows a trigger's body lists, `none` when the
# body declares zero components, or nothing when it has no marker at all.
_release_plugin_components_from_trigger() {
  local trigger_tag="$1"
  local body line tag sha extra count="" listed=0 seen=$'\n'
  local -a rows=()
  body="$(git tag -l --format='%(contents:body)' "${trigger_tag}")"
  while IFS= read -r line; do
    line="${line%$'\r'}"
    case "${line}" in
      components\ *)
        read -r _ count extra <<<"${line}"
        if [[ ! "${count}" =~ ^[0-9]+$ || -n "${extra:-}" ]]; then
          printf 'Malformed components marker in %s: %s\n' "${trigger_tag}" "${line}" >&2
          return 1
        fi
        ;;
      component\ *)
        read -r _ tag sha extra <<<"${line}"
        if [[ -z "${tag:-}" || -z "${sha:-}" || -n "${extra:-}" ]]; then
          printf 'Malformed component line in %s: %s\n' "${trigger_tag}" "${line}" >&2
          return 1
        fi
        if [[ "${seen}" == *$'\n'"${tag}"$'\n'* ]]; then
          printf 'Release trigger %s lists %s twice\n' "${trigger_tag}" "${tag}" >&2
          return 1
        fi
        seen+="${tag}"$'\n'
        rows+=("${tag}"$'\t'"${sha}")
        listed=$((listed + 1))
        ;;
    esac
  done <<<"${body}"
  [[ -n "${count}" ]] || return 0
  if (( count != listed )); then
    printf 'Release trigger %s declares %s components but lists %s\n' "${trigger_tag}" "${count}" "${listed}" >&2
    return 1
  fi
  if (( listed == 0 )); then
    printf 'none\n'
    return 0
  fi
  local row
  for row in "${rows[@]}"; do
    printf '%s\n' "${row}"
  done
}

# Prints the tagger email of a GitHub-verified signed tag.
_release_plugin_components_verify_tag() {
  local repository="$1" tag="$2" tag_object="${3:-}" verification verified tagger
  if [[ -z "${tag_object}" ]]; then
    tag_object="$(git rev-parse --verify --quiet "refs/tags/${tag}^{tag}")" || {
      printf '%s is not an annotated tag\n' "${tag}" >&2
      return 1
    }
  fi
  verification="$(gh api "repos/${repository}/git/tags/${tag_object}" --jq '[(.verification.verified | tostring), (.tagger.email // "")] | @tsv')"
  IFS=$'\t' read -r verified tagger <<<"${verification}"
  if [[ "${verified}" != true ]]; then
    printf '%s is not a GitHub-verified signed tag\n' "${tag}" >&2
    return 1
  fi
  printf '%s\n' "${tagger}"
}

_release_plugin_components_is_bot() {
  local tagger="$1" bot_email="$2"
  [[ -n "${bot_email}" && -n "${tagger}" ]] || return 1
  [[ "$(printf '%s' "${tagger}" | tr '[:upper:]' '[:lower:]')" == "$(printf '%s' "${bot_email}" | tr '[:upper:]' '[:lower:]')" ]]
}

# A bot-signed component must be an allowed plugin's stable patch release on
# a line that already has an earlier release: never a first release, a new
# minor or a new major. Whether it was newer than everything published is
# checked when the tags are created (xtask release-publish-tags --resume);
# this check stays true after later releases, so catalog-only republication
# of an older automated patch still passes.
_release_plugin_components_automated_release_allowed() {
  local plugin_id="$1" version="$2" tag="$3" prefix="$4"
  local allowed=false candidate
  for candidate in "${AUTOMATED_RELEASE_PLUGIN_IDS[@]}"; do
    [[ "${plugin_id}" == "${candidate}" ]] && allowed=true
  done
  if [[ "${allowed}" != true ]]; then
    printf 'Automated release bot may not release %s (%s); only %s\n' "${plugin_id}" "${tag}" "${AUTOMATED_RELEASE_PLUGIN_IDS[*]}" >&2
    return 1
  fi
  if [[ ! "${version}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    printf 'Automated release %s must be a stable x.y.z version\n' "${tag}" >&2
    return 1
  fi
  local major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"
  local other other_version
  while IFS= read -r other; do
    other_version="${other#"${prefix}/${plugin_id}/v"}"
    [[ "${other_version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || continue
    if [[ "${BASH_REMATCH[1]}" == "${major}" && "${BASH_REMATCH[2]}" == "${minor}" ]] && (( BASH_REMATCH[3] < patch )); then
      return 0
    fi
  done < <(git tag --list "${prefix}/${plugin_id}/v*")
  printf 'Automated release %s must be a patch bump on an already released %s.%s line\n' "${tag}" "${major}" "${minor}" >&2
  return 1
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  release_plugin_components "$@"
fi
