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
// sync (Commit, DefinitionsTree and each source's Tree); TrackedVersion,
// each source's Dir and SchemaBlob, and AcknowledgedVersionDirs are carried
// forward verbatim and only change in a reviewed human commit.
type cardigannUpstreamRecord struct {
	Repository string `json:"repository"`
	// Commit and DefinitionsTree are empty until the first recorded sync.
	Commit          string `json:"commit"`
	DefinitionsTree string `json:"definitions_tree"`
	// TrackedVersion is the newest definitions/<vN> the engine reads, the
	// Cardigann schema version it implements.
	TrackedVersion string `json:"tracked_version"`
	// SourceVersions are the definitions/<vN> directories the corpus draws
	// from, oldest first and ending at TrackedVersion. Upstream keeps each
	// definition only in the lowest schema version it validates against, so
	// the corpus takes each one from the highest of these that has it.
	SourceVersions []cardigannSourceVersion `json:"source_versions"`
	// AcknowledgedVersionDirs lists newer definitions/<vN> directories a
	// maintainer has reviewed and chosen not to adopt yet; any other newer
	// directory fails the refresh.
	AcknowledgedVersionDirs []string `json:"acknowledged_version_dirs"`
}

// cardigannSourceVersion is one definitions/<vN> the corpus draws from.
type cardigannSourceVersion struct {
	Dir string `json:"dir"`
	// Tree is empty until the first recorded sync.
	Tree       string `json:"tree"`
	SchemaBlob string `json:"schema_blob"`
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
	if err := validateCardigannRecord(record); err != nil {
		return record, fmt.Errorf("%s: %w", path, err)
	}
	return record, nil
}

func validateCardigannRecord(record cardigannUpstreamRecord) error {
	if record.Repository != cardigannUpstream {
		return fmt.Errorf("repository is %q, want %q", record.Repository, cardigannUpstream)
	}
	tracked, err := versionNumber(record.TrackedVersion)
	if err != nil {
		return fmt.Errorf("tracked_version: %w", err)
	}
	if len(record.SourceVersions) == 0 {
		return errors.New("source_versions must list the definitions directories the corpus draws from")
	}
	previous := 0
	for _, source := range record.SourceVersions {
		number, err := versionNumber(source.Dir)
		if err != nil {
			return fmt.Errorf("source_versions: %w", err)
		}
		if number <= previous {
			return errors.New("source_versions must be distinct and ordered oldest first")
		}
		previous = number
		if !gitObjectPattern.MatchString(source.SchemaBlob) {
			return fmt.Errorf("source_versions %s: schema_blob must be a 40-hex git object id", source.Dir)
		}
		if source.Tree != "" && !gitObjectPattern.MatchString(source.Tree) {
			return fmt.Errorf("source_versions %s: tree must be empty or a 40-hex git object id", source.Dir)
		}
	}
	if previous != tracked {
		return fmt.Errorf("source_versions must end at tracked_version %s", record.TrackedVersion)
	}
	for _, dir := range record.AcknowledgedVersionDirs {
		number, err := versionNumber(dir)
		if err != nil {
			return fmt.Errorf("acknowledged_version_dirs: %w", err)
		}
		if number <= tracked {
			return fmt.Errorf("acknowledged_version_dirs: %s is not newer than tracked_version %s", dir, record.TrackedVersion)
		}
	}
	return nil
}

func versionNumber(dir string) (int, error) {
	match := versionDirPattern.FindStringSubmatch(dir)
	if match == nil {
		return 0, fmt.Errorf("%q is not a definitions version directory (vN)", dir)
	}
	return strconv.Atoi(match[1])
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

// versionTreeFlags collects repeated `--version-tree vN=listing.json` flags.
// The workflow passes one for each source directory that exists upstream; a
// missing one is then reported by the guard.
type versionTreeFlags map[string]string

func (f versionTreeFlags) String() string { return "" }

func (f versionTreeFlags) Set(value string) error {
	dir, path, ok := strings.Cut(value, "=")
	if !ok || path == "" {
		return fmt.Errorf("--version-tree %q: want vN=path", value)
	}
	if _, err := versionNumber(dir); err != nil {
		return fmt.Errorf("--version-tree: %w", err)
	}
	if _, seen := f[dir]; seen {
		return fmt.Errorf("--version-tree %s passed twice", dir)
	}
	f[dir] = path
	return nil
}

func (f versionTreeFlags) read() (map[string]gitTreeListing, error) {
	listings := make(map[string]gitTreeListing, len(f))
	for dir, path := range f {
		listing, err := readTreeListing(path)
		if err != nil {
			return nil, err
		}
		listings[dir] = listing
	}
	return listings, nil
}

// upstreamObservation is what the guard derived from the upstream listings,
// in the form the record stores.
type upstreamObservation struct {
	DefinitionsTree string
	// Sources mirrors the record's SourceVersions with the observed trees.
	Sources []cardigannSourceVersion
}

// checkCardigannUpstream refuses an upstream whose definition layout moved
// since the recorded sync: a definitions/<vN> newer than the tracked version
// that nobody acknowledged, a source version directory disappearing, an
// unexpected entry in definitions/, or a changed schema.json in any source
// directory. Every problem found is reported, each naming exactly what moved.
// versions holds the listing of each source directory that exists upstream.
func checkCardigannUpstream(record cardigannUpstreamRecord, definitions gitTreeListing, versions map[string]gitTreeListing) (upstreamObservation, error) {
	observation := upstreamObservation{DefinitionsTree: definitions.SHA}
	if err := validateCardigannRecord(record); err != nil {
		return observation, err
	}
	trackedNumber, _ := versionNumber(record.TrackedVersion)
	acknowledged := make(map[string]struct{}, len(record.AcknowledgedVersionDirs))
	for _, dir := range record.AcknowledgedVersionDirs {
		acknowledged[dir] = struct{}{}
	}

	var problems []string
	upstreamTrees := map[string]string{}
	for _, entry := range definitions.Tree {
		number, err := versionNumber(entry.Path)
		if err != nil || entry.Type != "tree" {
			problems = append(problems, fmt.Sprintf("unexpected %s definitions/%s appeared upstream (%s)", entry.Type, entry.Path, entry.SHA))
			continue
		}
		upstreamTrees[entry.Path] = entry.SHA
		if number > trackedNumber {
			if _, ok := acknowledged[entry.Path]; !ok {
				problems = append(problems, fmt.Sprintf("newer definitions version definitions/%s exists upstream (tree %s); the engine reads up to definitions/%s", entry.Path, entry.SHA, record.TrackedVersion))
			}
		}
	}

	sourceDirs := map[string]struct{}{}
	for _, source := range record.SourceVersions {
		sourceDirs[source.Dir] = struct{}{}
		observed := cardigannSourceVersion{Dir: source.Dir, SchemaBlob: source.SchemaBlob}
		tree, ok := upstreamTrees[source.Dir]
		if !ok {
			problems = append(problems, fmt.Sprintf("definitions/%s, a source of the baked corpus, no longer exists upstream", source.Dir))
			observation.Sources = append(observation.Sources, observed)
			continue
		}
		observed.Tree = tree
		listing, ok := versions[source.Dir]
		if !ok {
			return observation, fmt.Errorf("definitions/%s exists upstream (tree %s), but no listing of it was passed", source.Dir, tree)
		}
		if listing.SHA != tree {
			return observation, fmt.Errorf("the definitions/%s listing is tree %s, but definitions/ names %s", source.Dir, listing.SHA, tree)
		}
		schema := ""
		for _, entry := range listing.Tree {
			if entry.Path == cardigannSchemaFile && entry.Type == "blob" {
				schema = entry.SHA
			}
		}
		switch {
		case schema == "":
			problems = append(problems, fmt.Sprintf("definitions/%s/%s no longer exists upstream", source.Dir, cardigannSchemaFile))
		case schema != source.SchemaBlob:
			problems = append(problems, fmt.Sprintf("definitions/%s/%s changed upstream: blob %s, recorded %s", source.Dir, cardigannSchemaFile, schema, source.SchemaBlob))
		}
		observation.Sources = append(observation.Sources, observed)
	}
	for dir := range versions {
		if _, ok := sourceDirs[dir]; !ok {
			return observation, fmt.Errorf("a listing was passed for definitions/%s, which is not a source of the baked corpus", dir)
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
	versionTrees := versionTreeFlags{}
	fs.Var(versionTrees, "version-tree", "vN=git/trees listing of a source definitions/<vN> that exists upstream (repeatable)")
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
	versions, err := versionTrees.read()
	if err != nil {
		return err
	}
	_, err = checkCardigannUpstream(record, definitions, versions)
	return err
}

// updatedCardigannRecord re-runs the guard and returns the record for a
// sync from commit, carrying the reviewed fields forward unchanged.
func updatedCardigannRecord(record cardigannUpstreamRecord, commit string, definitions gitTreeListing, versions map[string]gitTreeListing) (cardigannUpstreamRecord, error) {
	if !gitObjectPattern.MatchString(commit) {
		return record, fmt.Errorf("upstream commit %q is not a 40-hex sha", commit)
	}
	observation, err := checkCardigannUpstream(record, definitions, versions)
	if err != nil {
		return record, err
	}
	record.Commit = commit
	record.DefinitionsTree = observation.DefinitionsTree
	record.SourceVersions = observation.Sources
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
	versionTrees := versionTreeFlags{}
	fs.Var(versionTrees, "version-tree", "vN=git/trees listing of a source definitions/<vN> (repeatable)")
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
	versions, err := versionTrees.read()
	if err != nil {
		return err
	}
	updated, err := updatedCardigannRecord(record, *commit, definitions, versions)
	if err != nil {
		return err
	}
	data, err := json.MarshalIndent(updated, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(*recordPath, append(data, '\n'), 0o644)
}

// reviewedCardigannFields are the record fields only a reviewed human commit
// may change.
func reviewedCardigannFields(record cardigannUpstreamRecord) string {
	parts := []string{record.Repository, record.TrackedVersion, strings.Join(record.AcknowledgedVersionDirs, ",")}
	for _, source := range record.SourceVersions {
		parts = append(parts, source.Dir+"="+source.SchemaBlob)
	}
	return strings.Join(parts, "|")
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
	if reviewedCardigannFields(headRecord) != reviewedCardigannFields(baseRecord) {
		return fmt.Errorf("%s: a refresh may not change repository, tracked_version, acknowledged_version_dirs, or the source_versions directories and schema blobs", cardigannRecord)
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
