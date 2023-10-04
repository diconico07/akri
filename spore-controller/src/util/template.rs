use kube::core::DynamicObject;
use serde_json::Value;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Liquid Json error: {0:?}")]
    Template(#[from] liquid_json::Error),

    #[error("JSON (de)serialization error: {0:?}")]
    Serde(#[from] serde_json::Error),
}

pub fn render_object(obj: &DynamicObject, data: &Value) -> Result<DynamicObject, Error> {
    let tmpl = liquid_json::LiquidJson::new(serde_json::to_value(obj)?);
    Ok(serde_json::from_value::<DynamicObject>(tmpl.render(data)?)?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_render_object() {
        let template: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "{{ NAME }}-cm",
                "labels": {
                    "akri.sh/{{ NAME }}": "bar",
                },
            },
            "data": "{{ INSTANCES | each: '\"{{el.NAME}}\":\"{{el.PROPERTIES.DESCRIPTION}}\"' | join: ',' | prepend: '{' | append: '}' | json | output }}",
        })).unwrap();
        let vars = json!({
            "NAME": "foo",
            "INSTANCES": [
                {"NAME": "foo-1", "PROPERTIES": {"DESCRIPTION": "bar1"}},
                {"NAME": "foo-2", "PROPERTIES": {"DESCRIPTION": "bar2", "IGNORED": "ignored"}},
            ]
        });
        let expected: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "foo-cm",
                "labels": {
                    "akri.sh/foo": "bar",
                },
            },
            "data": {
                "foo-1": "bar1",
                "foo-2": "bar2",
            },
        }))
        .unwrap();

        assert_eq!(expected, render_object(&template, &vars).unwrap())
    }
}
