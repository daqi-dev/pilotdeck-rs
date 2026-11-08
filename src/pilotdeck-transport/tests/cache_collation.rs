use pilotdeck_agent::{ports::Route,request_assembly::cache_plan_with_collation};
use pilotdeck_protocol::{JsObject,JsString,JsValue,parse_json,stringify_json};
use pilotdeck_transport::node_collation::{node_collator,node_locale};
use std::cmp::Ordering;

fn array(value:&JsValue)->&[JsValue] { let JsValue::Array(values)=value else {panic!("array")};values }
fn field<'a>(value:&'a JsValue,key:&str)->&'a JsValue {value.get(key).unwrap()}
fn encoded(value:&JsValue)->String {stringify_json(value).unwrap().unwrap()}
#[test]
fn real_node_ten_default_locales_match_cache_order_and_seventy_fingerprints() {
    let oracle=parse_json(include_str!("fixtures/cache-collation/oracle.json")).unwrap();
    let strings=array(field(&oracle,"strings"));
    let inputs=array(field(&oracle,"inputs"));
    let mut pairs=0;let mut fingerprints=0;
    for profile in array(field(&oracle,"locales")) {
        let lang=field(profile,"LANG").as_string().unwrap().to_utf8().unwrap();
        let locale=node_locale(|key|(key=="LANG").then(||lang.clone()));
        assert_eq!(locale.to_string(),field(field(profile,"resolved"),"locale").as_string().unwrap().to_utf8().unwrap(),"{lang}");
        let collator=node_collator(&locale,true).unwrap();
        let compare=|left:&JsString,right:&JsString|collator.compare_utf16(left.units(),right.units());
        let signs=array(field(profile,"signs"));
        for (i,left) in strings.iter().enumerate() {
            for (j,right) in strings.iter().enumerate() {
                let actual=match compare(left.as_string().unwrap(),right.as_string().unwrap()) {Ordering::Less=>-1.0,Ordering::Equal=>0.0,Ordering::Greater=>1.0};
                assert_eq!(actual,array(&signs[i])[j].as_number().unwrap(),"LANG={lang} pair {i}/{j}");
                pairs+=1;
            }
        }
        let results=array(field(profile,"results"));
        for (index,row) in inputs.iter().enumerate() {
            let input=field(row,"input");
            let messages=array(field(input,"messages"));
            let route=Route {provider:field(input,"provider").as_string().unwrap().clone(),model:field(input,"model").as_string().unwrap().clone()};
            let mut prepared=input.as_object().unwrap().clone();
            let mut cache=JsObject::new();cache.insert("generation".into(),field(row,"generation").clone());
            prepared.insert("cachePlan".into(),JsValue::Object(cache));
            let original_tools=field(input,"tools").clone();
            let prepared=JsValue::Object(prepared);
            let actual=cache_plan_with_collation(&prepared,messages,&route,&compare).unwrap();
            // The fixed TS artifact records JSON output, not JS own-property
            // descriptors. Compare that entire exact projection and every hash.
            assert_eq!(encoded(&actual),encoded(&results[index]),"LANG={lang} case {:?}",field(row,"id"));
            assert_eq!(prepared.get("tools"),Some(&original_tools),"fingerprinting must not reorder the actual request catalog");
            fingerprints+=1;
        }
    }
    assert_eq!((pairs,fingerprints),(118_810,70));
}
