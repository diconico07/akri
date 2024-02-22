#[cfg(test)]
pub mod config_for_tests {
    use akri_shared::{
        akri::instance::{InstanceList, InstanceSpec},
        k8s::MockKubeInterface,
        os::file,
    };
    use log::trace;

    const LIST_PREFIX: &str = r#"
{
    "apiVersion": "v1",
    "items": ["#;
    const LIST_SUFFIX: &str = r#"
    ],
    "kind": "List",
    "metadata": {
        "resourceVersion": "",
        "selfLink": ""
    }
}"#;
    fn listify_kube_object(node_json: &str) -> String {
        format!("{}\n{}\n{}", LIST_PREFIX, node_json, LIST_SUFFIX)
    }

    pub fn configure_get_instances(
        mock: &mut MockKubeInterface,
        result_file: &'static str,
        listify_result: bool,
    ) {
        trace!("mock.expect_get_instances");
        mock.expect_get_instances().times(1).returning(move || {
            let json = file::read_file_to_string(result_file);
            let instance_list_json = if listify_result {
                listify_kube_object(&json)
            } else {
                json
            };
            let list: InstanceList = serde_json::from_str(&instance_list_json).unwrap();
            Ok(list)
        });
    }

    pub fn configure_update_instance(
        mock: &mut MockKubeInterface,
        instance_to_update: InstanceSpec,
        instance_name: &'static str,
        result_error: bool,
    ) {
        trace!(
            "mock.expect_update_instance name:{} error:{}",
            instance_name,
            result_error
        );
        mock.expect_update_instance()
            .times(1)
            .withf(move |instance, name| {
                name == instance_name
                    && instance.nodes == instance_to_update.nodes
                    && instance.device_usage == instance_to_update.device_usage
            })
            .returning(move |_, _| {
                if result_error {
                    Err(None.ok_or_else(|| anyhow::anyhow!("failure"))?)
                } else {
                    Ok(())
                }
            });
    }
}
