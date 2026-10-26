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
	testTrackedTree     = "2222222222222222222222222222222222222222"
	testSchemaBlob      = "3333333333333333333333333333333333333333"
	testNewerTree       = "4444444444444444444444444444444444444444"
	testOtherBlob       = "5555555555555555555555555555555555555555"
	testCommit          = "6666666666666666666666666666666666666666"
)

func testRecord() cardigannUpstreamRecord {
	return cardigannUpstreamRecord{
		Repository:     cardigannUpstream,
		TrackedVersion: "v11",
		SchemaBlob:     testSchemaBlob,
	}
}

func definitionsListing(entries ...gitTreeEntry) gitTreeListing {
	return gitTreeListing{SHA: testDefinitionsTree, Tree: entries}
}

func trackedListing(schema string) gitTreeListing {
	entries := []gitTreeEntry{{Path: "1337x.yml", Type: "blob", SHA: testOtherBlob}}
	if schema != "" {
		entries = append(entries, gitTreeEntry{Path: "schema.json", Type: "blob", SHA: schema})
	}
	return gitTreeListing{SHA: testTrackedTree, Tree: entries}
}

func versionTree(name, sha string) gitTreeEntry {
	return gitTreeEntry{Path: name, Type: "tree", SHA: sha}
}

func TestCardigannGuardAcceptsAnUnchangedLayout(t *testing.T) {
	observed, err := checkCardigannUpstream(
		testRecord(),
		definitionsListing(versionTree("v1", testOtherBlob), versionTree("v10", testOtherBlob), versionTree("v11", testTrackedTree)),
		trackedListing(testSchemaBlob),
	)
	if err != nil {
		t.Fatal(err)
	}
	if observed.DefinitionsTree != testDefinitionsTree || observed.TrackedVersionTree != testTrackedTree || observed.SchemaBlob != testSchemaBlob {
		t.Fatalf("unexpected observation %+v", observed)
	}
}

func TestCardigannGuardRefusesEachLayoutMove(t *testing.T) {
	cases := []struct {
		name        string
		record      func(*cardigannUpstreamRecord)
		definitions gitTreeListing
		tracked     gitTreeListing
		want        []string
	}{
		{
			name:        "newer version directory",
			definitions: definitionsListing(versionTree("v11", testTrackedTree), versionTree("v12", testNewerTree)),
			tracked:     trackedListing(testSchemaBlob),
			want:        []string{"definitions/v12 exists upstream (tree " + testNewerTree + ")", "tracks definitions/v11"},
		},
		{
			name:        "much newer version directory",
			definitions: definitionsListing(versionTree("v11", testTrackedTree), versionTree("v20", testNewerTree)),
			tracked:     trackedListing(testSchemaBlob),
			want:        []string{"definitions/v20 exists upstream"},
		},
		{
			name:        "tracked version gone",
			definitions: definitionsListing(versionTree("v10", testOtherBlob)),
			tracked:     trackedListing(testSchemaBlob),
			want:        []string{"tracked definitions/v11 no longer exists upstream"},
		},
		{
			name:        "schema changed",
			definitions: definitionsListing(versionTree("v11", testTrackedTree)),
			tracked:     trackedListing(testOtherBlob),
			want:        []string{"definitions/v11/schema.json changed upstream: blob " + testOtherBlob + ", recorded " + testSchemaBlob},
		},
		{
			name:        "schema removed",
			definitions: definitionsListing(versionTree("v11", testTrackedTree)),
			tracked:     trackedListing(""),
			want:        []string{"definitions/v11/schema.json no longer exists upstream"},
		},
		{
			name:        "unexpected entry",
			definitions: definitionsListing(versionTree("v11", testTrackedTree), gitTreeEntry{Path: "README.md", Type: "blob", SHA: testOtherBlob}),
			tracked:     trackedListing(testSchemaBlob),
			want:        []string{"unexpected blob definitions/README.md appeared upstream"},
		},
		{
			name:        "every move is reported at once",
			definitions: definitionsListing(versionTree("v11", testTrackedTree), versionTree("v12", testNewerTree)),
			tracked:     trackedListing(testOtherBlob),
			want:        []string{"definitions/v12 exists upstream", "schema.json changed upstream"},
		},
		{
			name:        "acknowledging one newer version does not admit another",
			record:      func(r *cardigannUpstreamRecord) { r.AcknowledgedVersionDirs = []string{"v12"} },
			definitions: definitionsListing(versionTree("v11", testTrackedTree), versionTree("v12", testNewerTree), versionTree("v13", testNewerTree)),
			tracked:     trackedListing(testSchemaBlob),
			want:        []string{"definitions/v13 exists upstream"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			record := testRecord()
			if tc.record != nil {
				tc.record(&record)
			}
			_, err := checkCardigannUpstream(record, tc.definitions, tc.tracked)
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
			if strings.Contains(tc.name, "acknowledging") && strings.Contains(err.Error(), "v12") {
				t.Fatalf("acknowledged v12 was still reported: %v", err)
			}
		})
	}
}

func TestCardigannGuardAcceptsAnAcknowledgedNewerVersion(t *testing.T) {
	record := testRecord()
	record.AcknowledgedVersionDirs = []string{"v12"}
	if _, err := checkCardigannUpstream(record, definitionsListing(versionTree("v11", testTrackedTree), versionTree("v12", testNewerTree)), trackedListing(testSchemaBlob)); err != nil {
		t.Fatal(err)
	}
}

func TestCardigannGuardRejectsAMismatchedTrackedListing(t *testing.T) {
	tracked := trackedListing(testSchemaBlob)
	tracked.SHA = testNewerTree
	if _, err := checkCardigannUpstream(testRecord(), definitionsListing(versionTree("v11", testTrackedTree)), tracked); err == nil || !strings.Contains(err.Error(), "definitions/ names") {
		t.Fatalf("accepted a tracked listing for another tree: %v", err)
	}
}

func TestCardigannRecordCarriesReviewedFieldsForward(t *testing.T) {
	record := testRecord()
	record.AcknowledgedVersionDirs = []string{"v12"}
	updated, err := updatedCardigannRecord(record, testCommit, definitionsListing(versionTree("v11", testTrackedTree), versionTree("v12", testNewerTree)), trackedListing(testSchemaBlob))
	if err != nil {
		t.Fatal(err)
	}
	if updated.Commit != testCommit || updated.DefinitionsTree != testDefinitionsTree || updated.TrackedVersionTree != testTrackedTree || updated.SchemaBlob != testSchemaBlob {
		t.Fatalf("observed fields not recorded: %+v", updated)
	}
	if updated.TrackedVersion != "v11" || len(updated.AcknowledgedVersionDirs) != 1 || updated.AcknowledgedVersionDirs[0] != "v12" {
		t.Fatalf("reviewed fields changed: %+v", updated)
	}
	if _, err := updatedCardigannRecord(record, "HEAD", definitionsListing(versionTree("v11", testTrackedTree)), trackedListing(testSchemaBlob)); err == nil {
		t.Fatal("accepted a non-sha upstream commit")
	}
	if _, err := updatedCardigannRecord(testRecord(), testCommit, definitionsListing(versionTree("v11", testTrackedTree)), trackedListing(testOtherBlob)); err == nil {
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
	definitions := write("definitions.json", definitionsListing(versionTree("v11", testTrackedTree)))
	tracked := write("tracked.json", trackedListing(testSchemaBlob))
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", definitions, "--tracked-tree", tracked}); err != nil {
		t.Fatal(err)
	}
	if err := cardigannRecordCommand([]string{"--record", recordPath, "--commit", testCommit, "--definitions-tree", definitions, "--tracked-tree", tracked}); err != nil {
		t.Fatal(err)
	}
	record, err := readCardigannRecord(recordPath)
	if err != nil {
		t.Fatal(err)
	}
	if record.DefinitionsTree != testDefinitionsTree || record.Commit != testCommit {
		t.Fatalf("record not rewritten: %+v", record)
	}
	gone := write("gone.json", definitionsListing(versionTree("v10", testOtherBlob)))
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", gone}); err == nil || !strings.Contains(err.Error(), "tracked definitions/v11 no longer exists") {
		t.Fatalf("missing tracked directory not reported: %v", err)
	}
	truncated := definitionsListing(versionTree("v11", testTrackedTree))
	truncated.Truncated = true
	if err := cardigannGuardCommand([]string{"--record", recordPath, "--definitions-tree", write("truncated.json", truncated), "--tracked-tree", tracked}); err == nil {
		t.Fatal("accepted a truncated listing")
	}
	if err := os.WriteFile(recordPath, []byte(`{"repository":"Prowlarr/Indexers","tracked_version":"v11","schema_blob":"`+testSchemaBlob+`","surprise":true}`), 0o600); err != nil {
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
	if record.TrackedVersion != "v11" {
		t.Fatalf("tracked version %q", record.TrackedVersion)
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

const testRecordJSON = `{"repository":"Prowlarr/Indexers","commit":"","definitions_tree":"","tracked_version":"v11","tracked_version_tree":"","schema_blob":"` + testSchemaBlob + `","acknowledged_version_dirs":[]}`

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
		{name: "reviewed record field", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, `"acknowledged_version_dirs":[]`, `"acknowledged_version_dirs":["v12"]`, 1), want: "may not change"},
		{name: "tracked version", manifest: goodManifest, lock: goodLock, record: strings.Replace(testRecordJSON, `"v11"`, `"v12"`, 1), want: "may not change"},
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
