package main

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const (
	testDefinitionsTree = "1111111111111111111111111111111111111111"
	testV11Tree         = "2222222222222222222222222222222222222222"
	testV11Schema       = "3333333333333333333333333333333333333333"
	testNewerTree       = "4444444444444444444444444444444444444444"
	testOtherBlob       = "5555555555555555555555555555555555555555"
	testCommit          = "6666666666666666666666666666666666666666"
	testV12Tree         = "7777777777777777777777777777777777777777"
	testV12Schema       = "8888888888888888888888888888888888888888"
)

func testRecord() cardigannUpstreamRecord {
	return cardigannUpstreamRecord{
		Repository:     cardigannUpstream,
		TrackedVersion: "v12",
		SourceVersions: []cardigannSourceVersion{
			{Dir: "v11", SchemaBlob: testV11Schema},
			{Dir: "v12", SchemaBlob: testV12Schema},
		},
	}
}

func definitionsListing(entries ...gitTreeEntry) gitTreeListing {
	return gitTreeListing{SHA: testDefinitionsTree, Tree: entries}
}

// sourceTrees is the upstream definitions/ layout the corpus expects, plus extra.
func sourceTrees(extra ...gitTreeEntry) gitTreeListing {
	entries := []gitTreeEntry{versionTree("v1", testOtherBlob), versionTree("v10", testOtherBlob), versionTree("v11", testV11Tree), versionTree("v12", testV12Tree)}
	return definitionsListing(append(entries, extra...)...)
}

func versionListing(tree, schema string) gitTreeListing {
	entries := []gitTreeEntry{{Path: "1337x.yml", Type: "blob", SHA: testOtherBlob}}
	if schema != "" {
		entries = append(entries, gitTreeEntry{Path: "schema.json", Type: "blob", SHA: schema})
	}
	return gitTreeListing{SHA: tree, Tree: entries}
}

func sourceListings() map[string]gitTreeListing {
	return map[string]gitTreeListing{
		"v11": versionListing(testV11Tree, testV11Schema),
		"v12": versionListing(testV12Tree, testV12Schema),
	}
}

func versionTree(name, sha string) gitTreeEntry {
	return gitTreeEntry{Path: name, Type: "tree", SHA: sha}
}

func TestCardigannGuardAcceptsAnUnchangedLayout(t *testing.T) {
	observed, err := checkCardigannUpstream(testRecord(), sourceTrees(), sourceListings())
	if err != nil {
		t.Fatal(err)
	}
	want := []cardigannSourceVersion{{Dir: "v11", Tree: testV11Tree, SchemaBlob: testV11Schema}, {Dir: "v12", Tree: testV12Tree, SchemaBlob: testV12Schema}}
	if observed.DefinitionsTree != testDefinitionsTree || len(observed.Sources) != 2 || observed.Sources[0] != want[0] || observed.Sources[1] != want[1] {
		t.Fatalf("unexpected observation %+v", observed)
	}
}

func TestCardigannGuardRefusesEachLayoutMove(t *testing.T) {
	withoutV11 := sourceListings()
	delete(withoutV11, "v11")
	withoutV12 := sourceListings()
	delete(withoutV12, "v12")
	listings := func(edit func(map[string]gitTreeListing)) map[string]gitTreeListing {
		out := sourceListings()
		edit(out)
		return out
	}
	cases := []struct {
		name        string
		record      func(*cardigannUpstreamRecord)
		definitions gitTreeListing
		versions    map[string]gitTreeListing
		want        []string
		notWant     string
	}{
		{
			name:        "newer version directory",
			definitions: sourceTrees(versionTree("v13", testNewerTree)),
			versions:    sourceListings(),
			want:        []string{"definitions/v13 exists upstream (tree " + testNewerTree + ")", "reads up to definitions/v12"},
		},
		{
			name:        "much newer version directory",
			definitions: sourceTrees(versionTree("v20", testNewerTree)),
			versions:    sourceListings(),
			want:        []string{"definitions/v20 exists upstream"},
		},
		{
			name:        "v11 gone",
			definitions: definitionsListing(versionTree("v10", testOtherBlob), versionTree("v12", testV12Tree)),
			versions:    withoutV11,
			want:        []string{"definitions/v11, a source of the baked corpus, no longer exists upstream"},
		},
		{
			name:        "v12 gone",
			definitions: definitionsListing(versionTree("v11", testV11Tree)),
			versions:    withoutV12,
			want:        []string{"definitions/v12, a source of the baked corpus, no longer exists upstream"},
		},
		{
			name:        "v11 schema changed",
			definitions: sourceTrees(),
			versions:    listings(func(m map[string]gitTreeListing) { m["v11"] = versionListing(testV11Tree, testOtherBlob) }),
			want:        []string{"definitions/v11/schema.json changed upstream: blob " + testOtherBlob + ", recorded " + testV11Schema},
			notWant:     "v12/schema.json",
		},
		{
			name:        "v12 schema changed",
			definitions: sourceTrees(),
			versions:    listings(func(m map[string]gitTreeListing) { m["v12"] = versionListing(testV12Tree, testOtherBlob) }),
			want:        []string{"definitions/v12/schema.json changed upstream: blob " + testOtherBlob + ", recorded " + testV12Schema},
			notWant:     "v11/schema.json",
		},
		{
			name:        "schema removed",
			definitions: sourceTrees(),
			versions:    listings(func(m map[string]gitTreeListing) { m["v12"] = versionListing(testV12Tree, "") }),
			want:        []string{"definitions/v12/schema.json no longer exists upstream"},
		},
		{
			name:        "unexpected entry",
			definitions: sourceTrees(gitTreeEntry{Path: "README.md", Type: "blob", SHA: testOtherBlob}),
			versions:    sourceListings(),
			want:        []string{"unexpected blob definitions/README.md appeared upstream"},
		},
		{
			name:        "every move is reported at once",
			definitions: definitionsListing(versionTree("v12", testV12Tree), versionTree("v13", testNewerTree)),
			versions: listings(func(m map[string]gitTreeListing) {
				delete(m, "v11")
				m["v12"] = versionListing(testV12Tree, testOtherBlob)
			}),
			want: []string{"definitions/v13 exists upstream", "definitions/v11, a source", "v12/schema.json changed upstream"},
		},
		{
			name:        "acknowledging one newer version does not admit another",
			record:      func(r *cardigannUpstreamRecord) { r.AcknowledgedVersionDirs = []string{"v13"} },
			definitions: sourceTrees(versionTree("v13", testNewerTree), versionTree("v14", testNewerTree)),
			versions:    sourceListings(),
			want:        []string{"definitions/v14 exists upstream"},
			notWant:     "v13",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			record := testRecord()
			if tc.record != nil {
				tc.record(&record)
			}
			_, err := checkCardigannUpstream(record, tc.definitions, tc.versions)
			if err == nil {
				t.Fatal("guard accepted a moved layout")
			}
			if !strings.Contains(err.Error(), "refusing to sync") {
				t.Fatalf("unexpected error %v", err)
			}
			for _, want := range tc.want {
				if !strings.Contains(err.Error(), want) {
					t.Fatalf("error %q does not say %q", err, want)
				}
			}
			if tc.notWant != "" && strings.Contains(err.Error(), tc.notWant) {
				t.Fatalf("error %q wrongly names %q", err, tc.notWant)
			}
		})
	}
}

func TestCardigannGuardAcceptsTheSourceVersionsAndAnAcknowledgedNewerOne(t *testing.T) {
	// v12 is a source, not a newer version, so it needs no acknowledgement.
	if _, err := checkCardigannUpstream(testRecord(), sourceTrees(), sourceListings()); err != nil {
		t.Fatal(err)
	}
	record := testRecord()
	record.AcknowledgedVersionDirs = []string{"v13"}
	if _, err := checkCardigannUpstream(record, sourceTrees(versionTree("v13", testNewerTree)), sourceListings()); err != nil {
		t.Fatal(err)
	}
}

func TestCardigannGuardRejectsMismatchedOrMissingListings(t *testing.T) {
	mismatched := sourceListings()
	mismatched["v12"] = versionListing(testNewerTree, testV12Schema)
	if _, err := checkCardigannUpstream(testRecord(), sourceTrees(), mismatched); err == nil || !strings.Contains(err.Error(), "definitions/ names") {
		t.Fatalf("accepted a v12 listing for another tree: %v", err)
	}
	missing := sourceListings()
	delete(missing, "v11")
	if _, err := checkCardigannUpstream(testRecord(), sourceTrees(), missing); err == nil || !strings.Contains(err.Error(), "no listing of it was passed") {
		t.Fatalf("accepted a missing v11 listing: %v", err)
	}
	extra := sourceListings()
	extra["v10"] = versionListing(testOtherBlob, testOtherBlob)
	if _, err := checkCardigannUpstream(testRecord(), sourceTrees(), extra); err == nil || !strings.Contains(err.Error(), "not a source") {
		t.Fatalf("accepted a listing for a non-source directory: %v", err)
	}
}

func TestCardigannRecordValidation(t *testing.T) {
	cases := map[string]func(*cardigannUpstreamRecord){
		"no sources": func(r *cardigannUpstreamRecord) { r.SourceVersions = nil },
		"unordered sources": func(r *cardigannUpstreamRecord) {
			r.SourceVersions[0], r.SourceVersions[1] = r.SourceVersions[1], r.SourceVersions[0]
		},
		"sources end below tracked":   func(r *cardigannUpstreamRecord) { r.TrackedVersion = "v13" },
		"bad schema blob":             func(r *cardigannUpstreamRecord) { r.SourceVersions[1].SchemaBlob = "HEAD" },
		"bad tree":                    func(r *cardigannUpstreamRecord) { r.SourceVersions[0].Tree = "nope" },
		"acknowledged older version":  func(r *cardigannUpstreamRecord) { r.AcknowledgedVersionDirs = []string{"v12"} },
		"acknowledged bad name":       func(r *cardigannUpstreamRecord) { r.AcknowledgedVersionDirs = []string{"latest"} },
		"another upstream repository": func(r *cardigannUpstreamRecord) { r.Repository = "someone/Indexers" },
	}
	for name, edit := range cases {
		t.Run(name, func(t *testing.T) {
			record := testRecord()
			edit(&record)
			if err := validateCardigannRecord(record); err == nil {
				t.Fatal("accepted an invalid record")
			}
		})
	}
	if err := validateCardigannRecord(testRecord()); err != nil {
		t.Fatal(err)
	}
}

func TestCardigannRecordCarriesReviewedFieldsForward(t *testing.T) {
	record := testRecord()
	record.AcknowledgedVersionDirs = []string{"v13"}
	updated, err := updatedCardigannRecord(record, testCommit, sourceTrees(versionTree("v13", testNewerTree)), sourceListings())
	if err != nil {
		t.Fatal(err)
	}
	if updated.Commit != testCommit || updated.DefinitionsTree != testDefinitionsTree ||
		updated.SourceVersions[0].Tree != testV11Tree || updated.SourceVersions[1].Tree != testV12Tree {
		t.Fatalf("observed fields not recorded: %+v", updated)
	}
	if updated.TrackedVersion != "v12" || updated.SourceVersions[0].SchemaBlob != testV11Schema || updated.SourceVersions[1].SchemaBlob != testV12Schema ||
		len(updated.AcknowledgedVersionDirs) != 1 || updated.AcknowledgedVersionDirs[0] != "v13" {
		t.Fatalf("reviewed fields changed: %+v", updated)
	}
	if _, err := updatedCardigannRecord(record, "HEAD", sourceTrees(), sourceListings()); err == nil {
		t.Fatal("accepted a non-sha upstream commit")
	}
	moved := sourceListings()
	moved["v11"] = versionListing(testV11Tree, testOtherBlob)
	if _, err := updatedCardigannRecord(testRecord(), testCommit, sourceTrees(), moved); err == nil {
		t.Fatal("recorded a moved schema")
	}
}

func TestCardigannRecordFileRoundTripAndCommandFiles(t *testing.T) {
	dir := t.TempDir()
	write := func(name string, value any) string {
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(dir, name)
		if err := os.WriteFile(path, data, 0o600); err != nil {
			t.Fatal(err)
		}
		return path
	}
	recordPath := write("record.json", testRecord())
	definitions := write("definitions.json", sourceTrees())
	v11 := "v11=" + write("v11.json", versionListing(testV11Tree, testV11Schema))
	v12 := "v12=" + write("v12.json", versionListing(testV12Tree, testV12Schema))
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", definitions, "--version-tree", v11, "--version-tree", v12}); err != nil {
		t.Fatal(err)
	}
	if err := cardigannRecordCommand([]string{"--record", recordPath, "--commit", testCommit, "--definitions-tree", definitions, "--version-tree", v11, "--version-tree", v12}); err != nil {
		t.Fatal(err)
	}
	record, err := readCardigannRecord(recordPath)
	if err != nil {
		t.Fatal(err)
	}
	if record.DefinitionsTree != testDefinitionsTree || record.Commit != testCommit || record.SourceVersions[1].Tree != testV12Tree {
		t.Fatalf("record not rewritten: %+v", record)
	}
	gone := write("gone.json", definitionsListing(versionTree("v10", testOtherBlob), versionTree("v12", testV12Tree)))
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", gone, "--version-tree", v12}); err == nil || !strings.Contains(err.Error(), "definitions/v11, a source of the baked corpus, no longer exists") {
		t.Fatalf("missing v11 directory not reported: %v", err)
	}
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", definitions, "--version-tree", v11, "--version-tree", v11}); err == nil {
		t.Fatal("accepted a repeated --version-tree")
	}
	truncated := sourceTrees()
	truncated.Truncated = true
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", write("truncated.json", truncated), "--version-tree", v11, "--version-tree", v12}); err == nil {
		t.Fatal("accepted a truncated listing")
	}
	if err := os.WriteFile(recordPath, []byte(`{"repository":"Prowlarr/Indexers","tracked_version":"v12","source_versions":[{"dir":"v12","tree":"","schema_blob":"`+testV12Schema+`"}],"surprise":true}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := readCardigannRecord(recordPath); err == nil {
		t.Fatal("accepted an unknown record field")
	}
}

func TestCommittedCardigannRecordParses(t *testing.T) {
	record, err := readCardigannRecord(filepath.Join("..", "..", cardigannRecord))
	if err != nil {
		t.Fatal(err)
	}
	if record.TrackedVersion != "v12" || len(record.SourceVersions) != 2 || record.SourceVersions[0].Dir != "v11" || record.SourceVersions[1].Dir != "v12" {
		t.Fatalf("unexpected committed record %+v", record)
	}
	if len(record.AcknowledgedVersionDirs) != 0 {
		t.Fatalf("acknowledged_version_dirs %v", record.AcknowledgedVersionDirs)
	}
	// Empty until the first recorded sync, so that sync counts as moved.
	if record.DefinitionsTree != "" {
		t.Fatalf("seeded definitions_tree %q", record.DefinitionsTree)
	}
}

const testManifest = `[package]
name = "cardigann-engine"
version = "0.4.1"
edition = "2024"

[dependencies]
serde = { version = "1.0.0" }
`

const testLock = `version = 4

[[package]]
name = "base64"
version = "0.23.1"

[[package]]
name = "cardigann-engine"
version = "0.4.1"
dependencies = [
 "base64",
]
`

const testRecordJSON = `{"repository":"Prowlarr/Indexers","commit":"","definitions_tree":"","tracked_version":"v12","source_versions":[{"dir":"v11","tree":"","schema_blob":"` + testV11Schema + `"},{"dir":"v12","tree":"","schema_blob":"` + testV12Schema + `"}],"acknowledged_version_dirs":[]}`

type fakeRevisions map[string]map[string]string

func (f fakeRevisions) show(rev, path string) ([]byte, error) {
	content, ok := f[rev][path]
	if !ok {
		return nil, errors.New("missing")
	}
	return []byte(content), nil
}

func cardigannRevisions(headManifest, headLock, headRecord string) fakeRevisions {
	return fakeRevisions{
		"base":      {cardigannManifest: testManifest, cardigannLockfile: testLock, cardigannRecord: testRecordJSON},
		workingTree: {cardigannManifest: headManifest, cardigannLockfile: headLock, cardigannRecord: headRecord},
	}
}

var cardigannRefreshPaths = []string{cardigannIndexAsset, cardigannBodiesAsset, cardigannRecord, cardigannManifest, cardigannLockfile}

func bumped(text, from, to string) string { return strings.Replace(text, from, to, 1) }

func TestCardigannDiffAcceptsAPatchRefresh(t *testing.T) {
	headManifest := bumped(testManifest, `version = "0.4.1"`, `version = "0.4.2"`)
	headLock := bumped(testLock, "name = \"cardigann-engine\"\nversion = \"0.4.1\"", "name = \"cardigann-engine\"\nversion = \"0.4.2\"")
	headRecord := strings.Replace(testRecordJSON, `"commit":""`, `"commit":"`+testCommit+`"`, 1)
	headRecord = strings.Replace(headRecord, `"dir":"v12","tree":""`, `"dir":"v12","tree":"`+testV12Tree+`"`, 1)
	revisions := cardigannRevisions(headManifest, headLock, headRecord)
	if err := verifyCardigannDiff("base", cardigannRefreshPaths, revisions.show); err != nil {
		t.Fatal(err)
	}
	if err := verifyCardigannDiff("base", []string{cardigannBodiesAsset, cardigannRecord, cardigannManifest, cardigannLockfile}, revisions.show); err != nil {
		t.Fatalf("a bodies-only corpus change was refused: %v", err)
	}
}

func TestCardigannDiffRefusesAnythingButOnePatchBump(t *testing.T) {
	goodManifest := bumped(testManifest, `version = "0.4.1"`, `version = "0.4.2"`)
	goodLock := bumped(testLock, "name = \"cardigann-engine\"\nversion = \"0.4.1\"", "name = \"cardigann-engine\"\nversion = \"0.4.2\"")
	cases := []struct {
		name                   string
		paths                  []string
		manifest, lock, record string
		want                   string
	}{
		{name: "minor bump", manifest: bumped(testManifest, `version = "0.4.1"`, `version = "0.5.0"`), lock: bumped(testLock, "name = \"cardigann-engine\"\nversion = \"0.4.1\"", "name = \"cardigann-engine\"\nversion = \"0.5.0\""), want: "single patch bump"},
		{name: "dependency edit", manifest: bumped(goodManifest, `version = "1.0.0"`, `version = "1.0.1"`), lock: goodLock, want: "more than one line"},
		{name: "dependency version only", manifest: bumped(testManifest, `version = "1.0.0"`, `version = "1.0.1"`), lock: goodLock, want: "not the crate version"},
		{name: "added manifest line", manifest: goodManifest + "build = \"build.rs\"\n", lock: goodLock, want: "added or removed"},
		{name: "lock bumps another package", manifest: goodManifest, lock: bumped(testLock, `version = "0.23.1"`, `version = "0.23.2"`), want: "not the crate version"},
		{name: "lock disagrees", manifest: goodManifest, lock: bumped(testLock, "name = \"cardigann-engine\"\nversion = \"0.4.1\"", "name = \"cardigann-engine\"\nversion = \"0.4.3\""), want: "but Cargo.toml moved"},
		{name: "reviewed record field", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, `"acknowledged_version_dirs":[]`, `"acknowledged_version_dirs":["v13"]`, 1), want: "may not change"},
		{name: "tracked version", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, `"tracked_version":"v12"`, `"tracked_version":"v13"`, 1), want: "may not change"},
		{name: "source schema blob", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, testV12Schema, testOtherBlob, 1), want: "may not change"},
		{name: "source directories", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, `{"dir":"v11","tree":"","schema_blob":"`+testV11Schema+`"},`, "", 1), want: "may not change"},
		{name: "no corpus change", paths: []string{cardigannRecord, cardigannManifest, cardigannLockfile}, manifest: goodManifest, lock: goodLock, want: "baked corpus"},
		{name: "no version change", paths: []string{cardigannIndexAsset, cardigannRecord}, manifest: goodManifest, lock: goodLock, want: "must change " + cardigannManifest},
		{name: "no record change", paths: []string{cardigannIndexAsset, cardigannManifest, cardigannLockfile}, manifest: goodManifest, lock: goodLock, want: "must change " + cardigannRecord},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			record := tc.record
			if record == "" {
				record = testRecordJSON
			}
			paths := tc.paths
			if paths == nil {
				paths = cardigannRefreshPaths
			}
			err := verifyCardigannDiff("base", paths, cardigannRevisions(tc.manifest, tc.lock, record).show)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("got %v, want an error containing %q", err, tc.want)
			}
		})
	}
}

func TestCardigannProfileAllowsOnlyItsRefreshOutputs(t *testing.T) {
	profile := profiles["cardigann"]
	if profile.ID != "" || profile.Verify == nil {
		t.Fatal("the cardigann profile must be a verified non-rule-pack profile")
	}
	if len(profile.Allowed) != len(cardigannRefreshPaths) {
		t.Fatalf("allowlist has %d paths", len(profile.Allowed))
	}
	for _, path := range cardigannRefreshPaths {
		if _, ok := profile.Allowed[path]; !ok {
			t.Fatalf("missing %s", path)
		}
	}
	for _, path := range []string{"indexers/cardigann-engine/src/baked.rs", "indexers/cardigann-engine/README.md", "xtask/src/main.rs"} {
		if _, ok := profile.Allowed[path]; ok {
			t.Fatalf("allowlists %s", path)
		}
	}
	if _, err := lookupRulePackProfile("cardigann"); err == nil {
		t.Fatal("cardigann accepted as a rule-pack profile")
	}
}

func TestGitShowReadsTheWorkingTreeFromTheRepositoryRoot(t *testing.T) {
	want, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatal(err)
	}
	got, err := gitShow(workingTree, "rule_packs/refresh/go.mod")
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(want) {
		t.Fatal("working-tree read differs from the file on disk")
	}
}
