package main

// The Cardigann definition refresh. Unlike the rule packs it regenerates a
// plugin's baked corpus, so besides the path allowlist its diff is held to a
// single patch bump of the crate's own version, and the upstream it syncs from
// is pinned by a committed record the daily workflow compares against before
// it builds anything.
//
// This helper still has no network client: the workflow fetches the upstream
// git tree listings (GitHub's `git/trees` API) and passes them in as files.

import (
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

const (
	cardigannCrateDir    = "indexers/cardigann-engine"
	cardigannManifest    = cardigannCrateDir + "/Cargo.toml"
	cardigannLockfile    = cardigannCrateDir + "/Cargo.lock"
	cardigannIndexAsset  = cardigannCrateDir + "/known_cardigann_definitions.v1.jsonl"
	cardigannBodiesAsset = cardigannCrateDir + "/known_cardigann_definitions.v1.bodies.jsonl"
	cardigannRecord      = cardigannCrateDir + "/known_cardigann_definitions.upstream.json"
	cardigannPackageName = "cardigann-engine"
	cardigannUpstream    = "Prowlarr/Indexers"
	cardigannSchemaFile  = "schema.json"
)

// cardigannUpstreamRecord is the committed identity of the upstream the baked
// corpus was last generated from. The bot rewrites the observed fields on each
// sync; TrackedVersion and AcknowledgedVersionDirs are carried forward
// verbatim and only change in a reviewed human commit.
type cardigannUpstreamRecord struct {
	Repository string `json:"repository"`
	// Commit and DefinitionsTree are empty until the first recorded sync.
	Commit          string `json:"commit"`
	DefinitionsTree string `json:"definitions_tree"`
	// TrackedVersion is the definitions/<vN> directory the engine bakes.
	TrackedVersion     string `json:"tracked_version"`
	TrackedVersionTree string `json:"tracked_version_tree"`
	SchemaBlob         string `json:"schema_blob"`
	// AcknowledgedVersionDirs lists newer definitions/<vN> directories a
	// maintainer has reviewed and chosen not to adopt yet; any other newer
	// directory fails the refresh.
	AcknowledgedVersionDirs []string `json:"acknowledged_version_dirs"`
}

// gitTreeListing is the subset of GitHub's `GET /repos/{o}/{r}/git/trees/{sha}`
// response the guard reads.
type gitTreeListing struct {
	SHA       string         `json:"sha"`
	Truncated bool           `json:"truncated"`
	Tree      []gitTreeEntry `json:"tree"`
}

type gitTreeEntry struct {
	Path string `json:"path"`
	Type string `json:"type"`
	SHA  string `json:"sha"`
}

var (
	versionDirPattern = regexp.MustCompile(`^v([1-9][0-9]*)$`)
	gitObjectPattern  = regexp.MustCompile(`^[0-9a-f]{40}$`)
)

func readCardigannRecord(path string) (cardigannUpstreamRecord, error) {
	var record cardigannUpstreamRecord
	data, err := os.ReadFile(path)
	if err != nil {
		return record, err
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&record); err != nil {
		return record, fmt.Errorf("%s: %w", path, err)
	}
	if record.Repository != cardigannUpstream {
		return record, fmt.Errorf("%s: repository is %q, want %q", path, record.Repository, cardigannUpstream)
	}
	if _, err := versionNumber(record.TrackedVersion); err != nil {
		return record, fmt.Errorf("%s: tracked_version: %w", path, err)
	}
	if !gitObjectPattern.MatchString(record.SchemaBlob) {
		return record, fmt.Errorf("%s: schema_blob must be a 40-hex git object id", path)
	}
	for _, dir := range record.AcknowledgedVersionDirs {
		if _, err := versionNumber(dir); err != nil {
			return record, fmt.Errorf("%s: acknowledged_version_dirs: %w", path, err)
		}
	}
	return record, nil
}

func versionNumber(dir string) (int, error) {
	match := versionDirPattern.FindStringSubmatch(dir)
	if match == nil {
		return 0, fmt.Errorf("%q is not a definitions version directory (vN)", dir)
	}
	return strconv.Atoi(match[1])
}

// readOptionalTreeListing reads a listing, or returns an empty one for an
// empty path: the workflow passes none when the tracked directory is absent,
// which the guard then reports.
func readOptionalTreeListing(path string) (gitTreeListing, error) {
	if path == "" {
		return gitTreeListing{}, nil
	}
	return readTreeListing(path)
}

func readTreeListing(path string) (gitTreeListing, error) {
	var listing gitTreeListing
	data, err := os.ReadFile(path)
	if err != nil {
		return listing, err
	}
	if err := json.Unmarshal(data, &listing); err != nil {
		return listing, fmt.Errorf("%s: %w", path, err)
	}
	if !gitObjectPattern.MatchString(listing.SHA) {
		return listing, fmt.Errorf("%s: tree listing has no 40-hex sha", path)
	}
	if listing.Truncated {
		return listing, fmt.Errorf("%s: tree listing %s is truncated", path, listing.SHA)
	}
	return listing, nil
}

// upstreamObservation is what the guard derived from the upstream listings,
// in the form the record stores.
type upstreamObservation struct {
	DefinitionsTree    string
	TrackedVersionTree string
	SchemaBlob         string
}

// checkCardigannUpstream refuses an upstream whose definition layout moved
// since the recorded sync: a definitions/<vN> newer than the tracked version
// that nobody acknowledged, the tracked version directory disappearing, an
// unexpected entry in definitions/, or a changed tracked schema. Every
// problem found is reported, each naming exactly what moved.
func checkCardigannUpstream(record cardigannUpstreamRecord, definitions, tracked gitTreeListing) (upstreamObservation, error) {
	observation := upstreamObservation{DefinitionsTree: definitions.SHA}
	trackedNumber, err := versionNumber(record.TrackedVersion)
	if err != nil {
		return observation, err
	}
	acknowledged := make(map[string]struct{}, len(record.AcknowledgedVersionDirs))
	for _, dir := range record.AcknowledgedVersionDirs {
		acknowledged[dir] = struct{}{}
	}

	var problems []string
	trackedTree := ""
	for _, entry := range definitions.Tree {
		number, err := versionNumber(entry.Path)
		if err != nil || entry.Type != "tree" {
			problems = append(problems, fmt.Sprintf("unexpected %s definitions/%s appeared upstream (%s)", entry.Type, entry.Path, entry.SHA))
			continue
		}
		switch {
		case number == trackedNumber:
			trackedTree = entry.SHA
		case number > trackedNumber:
			if _, ok := acknowledged[entry.Path]; !ok {
				problems = append(problems, fmt.Sprintf("newer definitions version definitions/%s exists upstream (tree %s); the engine tracks definitions/%s", entry.Path, entry.SHA, record.TrackedVersion))
			}
		}
	}
	if trackedTree == "" {
		problems = append(problems, fmt.Sprintf("tracked definitions/%s no longer exists upstream", record.TrackedVersion))
	} else {
		observation.TrackedVersionTree = trackedTree
		if tracked.SHA != trackedTree {
			return observation, fmt.Errorf("the definitions/%s listing is tree %s, but definitions/ names %s", record.TrackedVersion, tracked.SHA, trackedTree)
		}
		schema := ""
		for _, entry := range tracked.Tree {
			if entry.Path == cardigannSchemaFile && entry.Type == "blob" {
				schema = entry.SHA
			}
		}
		switch {
		case schema == "":
			problems = append(problems, fmt.Sprintf("definitions/%s/%s no longer exists upstream", record.TrackedVersion, cardigannSchemaFile))
		case schema != record.SchemaBlob:
			problems = append(problems, fmt.Sprintf("definitions/%s/%s changed upstream: blob %s, recorded %s", record.TrackedVersion, cardigannSchemaFile, schema, record.SchemaBlob))
		default:
			observation.SchemaBlob = schema
		}
	}
	if len(problems) > 0 {
		sort.Strings(problems)
		return observation, fmt.Errorf("the upstream Cardigann definition layout moved; refusing to sync:\n- %s", strings.Join(problems, "\n- "))
	}
	return observation, nil
}

func cardigannGuardCommand(args []string) error {
	fs := flag.NewFlagSet("cardigann-guard", flag.ContinueOnError)
	recordPath := fs.String("record", "", "committed upstream record")
	definitionsPath := fs.String("definitions-tree", "", "git/trees listing of upstream definitions/")
	trackedPath := fs.String("tracked-tree", "", "git/trees listing of the tracked definitions/<vN>, if it exists")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *recordPath == "" || *definitionsPath == "" {
		return errors.New("cardigann-guard requires --record and --definitions-tree")
	}
	record, err := readCardigannRecord(*recordPath)
	if err != nil {
		return err
	}
	definitions, err := readTreeListing(*definitionsPath)
	if err != nil {
		return err
	}
	tracked, err := readOptionalTreeListing(*trackedPath)
	if err != nil {
		return err
	}
	_, err = checkCardigannUpstream(record, definitions, tracked)
	return err
}

// updatedCardigannRecord re-runs the guard and returns the record for a
// sync from commit, carrying the reviewed fields forward unchanged.
func updatedCardigannRecord(record cardigannUpstreamRecord, commit string, definitions, tracked gitTreeListing) (cardigannUpstreamRecord, error) {
	if !gitObjectPattern.MatchString(commit) {
		return record, fmt.Errorf("upstream commit %q is not a 40-hex sha", commit)
	}
	observation, err := checkCardigannUpstream(record, definitions, tracked)
	if err != nil {
		return record, err
	}
	record.Commit = commit
	record.DefinitionsTree = observation.DefinitionsTree
	record.TrackedVersionTree = observation.TrackedVersionTree
	record.SchemaBlob = observation.SchemaBlob
	if record.AcknowledgedVersionDirs == nil {
		record.AcknowledgedVersionDirs = []string{}
	}
	return record, nil
}

func cardigannRecordCommand(args []string) error {
	fs := flag.NewFlagSet("cardigann-record", flag.ContinueOnError)
	recordPath := fs.String("record", "", "committed upstream record to rewrite")
	commit := fs.String("commit", "", "upstream commit the corpus was synced from")
	definitionsPath := fs.String("definitions-tree", "", "git/trees listing of upstream definitions/")
	trackedPath := fs.String("tracked-tree", "", "git/trees listing of the tracked definitions/<vN>, if it exists")
	if err := fs.Parse(args); err != nil {
		return err
	}
	record, err := readCardigannRecord(*recordPath)
	if err != nil {
		return err
	}
	definitions, err := readTreeListing(*definitionsPath)
	if err != nil {
		return err
	}
	tracked, err := readTreeListing(*trackedPath)
	if err != nil {
		return err
	}
	updated, err := updatedCardigannRecord(record, *commit, definitions, tracked)
	if err != nil {
		return err
	}
	data, err := json.MarshalIndent(updated, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(*recordPath, append(data, '\n'), 0o644)
}

// verifyCardigannDiff holds a refresh to one coherent patch release: the
// corpus and upstream record changed, and the only other edits are the crate
// version line in Cargo.toml and the crate's own entry in Cargo.lock, both
// moving to the next patch of the base version. It also refuses any change to
// the record's reviewed fields.
//
// Like the path check, it compares base with the working tree: the refresh runs
// it before committing, and publication runs it on a clean merge checkout.
func verifyCardigannDiff(base string, changed []string, show func(rev, path string) ([]byte, error)) error {
	changedSet := pathSet(changed...)
	for _, required := range []string{cardigannRecord, cardigannManifest, cardigannLockfile} {
		if _, ok := changedSet[required]; !ok {
			return fmt.Errorf("a Cardigann refresh must change %s", required)
		}
	}
	_, index := changedSet[cardigannIndexAsset]
	_, bodies := changedSet[cardigannBodiesAsset]
	if !index && !bodies {
		return errors.New("a Cardigann refresh must change the baked corpus")
	}
	read := func(rev, path string) ([]byte, error) {
		data, err := show(rev, path)
		if err != nil {
			return nil, fmt.Errorf("read %s at %q: %w", path, rev, err)
		}
		return data, nil
	}
	baseManifest, err := read(base, cardigannManifest)
	if err != nil {
		return err
	}
	headManifest, err := read(workingTree, cardigannManifest)
	if err != nil {
		return err
	}
	from, to, err := singleVersionLineChange(baseManifest, headManifest, isManifestVersionLine)
	if err != nil {
		return fmt.Errorf("%s: %w", cardigannManifest, err)
	}
	if want, err := nextPatch(from); err != nil || to != want {
		return fmt.Errorf("%s: version moved %s -> %s, want a single patch bump", cardigannManifest, from, to)
	}
	baseLock, err := read(base, cardigannLockfile)
	if err != nil {
		return err
	}
	headLock, err := read(workingTree, cardigannLockfile)
	if err != nil {
		return err
	}
	lockFrom, lockTo, err := singleVersionLineChange(baseLock, headLock, isLockSelfVersionLine)
	if err != nil {
		return fmt.Errorf("%s: %w", cardigannLockfile, err)
	}
	if lockFrom != from || lockTo != to {
		return fmt.Errorf("%s: crate entry moved %s -> %s, but Cargo.toml moved %s -> %s", cardigannLockfile, lockFrom, lockTo, from, to)
	}
	baseRecordData, err := read(base, cardigannRecord)
	if err != nil {
		return err
	}
	headRecordData, err := read(workingTree, cardigannRecord)
	if err != nil {
		return err
	}
	var baseRecord, headRecord cardigannUpstreamRecord
	if err := json.Unmarshal(baseRecordData, &baseRecord); err != nil {
		return fmt.Errorf("%s at %s: %w", cardigannRecord, base, err)
	}
	if err := json.Unmarshal(headRecordData, &headRecord); err != nil {
		return fmt.Errorf("%s in the working tree: %w", cardigannRecord, err)
	}
	if headRecord.Repository != baseRecord.Repository || headRecord.TrackedVersion != baseRecord.TrackedVersion ||
		strings.Join(headRecord.AcknowledgedVersionDirs, ",") != strings.Join(baseRecord.AcknowledgedVersionDirs, ",") {
		return fmt.Errorf("%s: a refresh may not change repository, tracked_version or acknowledged_version_dirs", cardigannRecord)
	}
	return nil
}

// workingTree is the revision verifyCardigannDiff passes to read the files as
// they are on disk.
const workingTree = ""

var versionValuePattern = regexp.MustCompile(`^version = "([^"]+)"$`)

// isManifestVersionLine accepts the [package] version line only: the first
// `version = "..."` line, which must come before any other table.
func isManifestVersionLine(lines []string, index int) bool {
	if !versionValuePattern.MatchString(lines[index]) {
		return false
	}
	section := ""
	for i := 0; i < index; i++ {
		line := strings.TrimSpace(lines[i])
		if strings.HasPrefix(line, "[") {
			section = line
		}
		if versionValuePattern.MatchString(lines[i]) {
			return false
		}
	}
	return section == "[package]"
}

// isLockSelfVersionLine accepts the version line of the crate's own
// [[package]] entry, which directly follows its name line.
func isLockSelfVersionLine(lines []string, index int) bool {
	return index > 0 && versionValuePattern.MatchString(lines[index]) &&
		lines[index-1] == fmt.Sprintf("name = %q", cardigannPackageName)
}

// singleVersionLineChange requires base and head to differ in exactly one
// line, accepted by allowed in both, and returns the versions it moved from
// and to.
func singleVersionLineChange(base, head []byte, allowed func([]string, int) bool) (string, string, error) {
	baseLines := strings.Split(string(base), "\n")
	headLines := strings.Split(string(head), "\n")
	if len(baseLines) != len(headLines) {
		return "", "", errors.New("lines were added or removed; only the crate version may change")
	}
	changed := -1
	for i := range baseLines {
		if baseLines[i] == headLines[i] {
			continue
		}
		if changed >= 0 {
			return "", "", errors.New("more than one line changed; only the crate version may change")
		}
		changed = i
	}
	if changed < 0 {
		return "", "", errors.New("the crate version did not change")
	}
	if !allowed(baseLines, changed) || !allowed(headLines, changed) {
		return "", "", fmt.Errorf("line %d changed, but it is not the crate version", changed+1)
	}
	from := versionValuePattern.FindStringSubmatch(baseLines[changed])[1]
	to := versionValuePattern.FindStringSubmatch(headLines[changed])[1]
	return from, to, nil
}
