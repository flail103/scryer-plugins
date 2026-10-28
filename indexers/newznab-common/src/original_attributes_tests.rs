use super::*;

#[test]
fn xml_originals_preserve_values_repetition_namespace_scope_and_item_boundaries() {
    let body = r#"<rss xmlns:n="http://www.newznab.com/DTD/2010/feeds/attributes/" xmlns:t="http://torznab.com/schemas/2015/feed"><channel>
      <item><title>First</title><n:attr name="seeders" value="007"/>
        <n:attr name="custom" value=" first &amp; &lt;second&gt; "></n:attr>
        <t:attr name="custom" value="again"/><category>4000</category>
      </item>
      <item xmlns:n="urn:fixture:local"><title>Second</title><n:attr name="custom" value="local"/></item>
    </channel></rss>"#;
    let (results, _, count) = parse_newznab_xml(body, 100, extract_base_metadata).unwrap();
    assert_eq!(count, 2);
    assert_eq!(results[0].attributes.len(), 4);
    let first = &results[0].attributes;
    assert_eq!(first[0].value, "007");
    assert_eq!(first[1].name, "custom");
    assert_eq!(first[1].value, " first & <second> ");
    assert_eq!(first[1].namespace.as_deref(), Some("n"));
    assert_eq!(
        first[1].namespace_uri.as_deref(),
        Some("http://www.newznab.com/DTD/2010/feeds/attributes/")
    );
    assert_eq!(first[2].name, "custom");
    assert_eq!(first[2].value, "again");
    assert_eq!(
        first[2].namespace_uri.as_deref(),
        Some("http://torznab.com/schemas/2015/feed")
    );
    assert_eq!(first[3].origin, PluginReleaseAttributeOrigin::Element);
    assert_eq!(first[3].name, "category");
    assert_eq!(first[3].value, "4000");
    assert_eq!(results[1].attributes.len(), 1);
    assert_eq!(
        results[1].attributes[0].namespace_uri.as_deref(),
        Some("urn:fixture:local")
    );
}

#[test]
fn json_originals_accept_plain_and_namespaced_groups_without_losing_duplicates() {
    let body = r#"{"channel":{"item":[{"title":"First",
      "attr":{"@attributes":{"name":"seeders","value":"007"}},
      "newznab:attr":[{"@attributes":{"name":"custom","value":"001.00"}},{"@attributes":{"name":"custom","value":"second & <third>"}}],
      "torznab:attr":{"@attributes":{"name":"peers","value":"009"}},
      "category":["4000","Custom"]
    }]}}"#;
    let (results, _, count) = parse_newznab_json(body, 100, extract_base_metadata).unwrap();
    assert_eq!(count, 1);
    let attrs = &results[0].attributes;
    assert_eq!(attrs.len(), 6);
    assert_eq!(attrs[0].value, "007");
    assert_eq!(attrs[0].namespace, None);
    assert_eq!(attrs[1].namespace.as_deref(), Some("newznab"));
    assert_eq!(attrs[1].name, "custom");
    assert_eq!(attrs[1].value, "001.00");
    assert_eq!(attrs[2].name, "custom");
    assert_eq!(attrs[2].value, "second & <third>");
    assert_eq!(attrs[3].namespace.as_deref(), Some("torznab"));
    assert_eq!(attrs[3].value, "009");
    assert_eq!(attrs[4].origin, PluginReleaseAttributeOrigin::Element);
    assert_eq!(attrs[5].value, "Custom");
    assert!(attrs.iter().all(|attr| attr.namespace_uri.is_none()));
}
