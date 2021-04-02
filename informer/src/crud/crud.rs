use kube_metrics_mutli_tenancy_lib as kube_lib;
use log::{debug,error,info};
use kube::{Api,Client,api::{Patch,PatchParams}};
use reqwest::Client as RClient;
use reqwest::{Response,Error};

use crate::rules::rules;


// Ruler rule modification actions should return 202 on success.
async fn check_response_202(response: Result<Response,Error>) {
    match response {
        Ok(r) => {
            let s = r.status();
            match r.text().await {
                Ok(t) => {
                    info!("received ruler response {}, text {}", s.to_string(), t);
                    assert_eq!(s, 202)
                },
                Err(e) => {
                    error!("failed to receive ruler response body: {}", e);
                }
            }
        },
        Err(e) => {
            error!("failed to update ruler, abort: {}", e);
        }
    }
}

// update ruler inside rule
pub async fn update_ruler_rule(
    client: RClient,
    ruler_api_url: &String,
    tenant_id: &String,
    namespace: &String,
    rule_group: kube_lib::GroupSpec) {
    let url = ruler_api_url.clone() + "api/v1/rules/" + &namespace.clone();
    debug!("ruler URL is {}, going to insert data", url);
    match serde_yaml::to_string(&rule_group) {
        Ok(body) => {
            debug!("ruler req body is {}", body);
            let response = client.post(&url)
                .header("X-Scope-OrgID", tenant_id)
                .body(body)
                .send()
                .await;
            check_response_202(response).await;

        },
        Err(e) => {
            error!("failed to encode rule group, abort: {}", e);
        }
    };
}

// remove rule from a Ruler
pub async fn remove_ruler_rule(
    client: RClient,
    ruler_api_url: &String,
    tenant_id: &String,
    namespace: &String,
    rule_group: kube_lib::GroupSpec) {
    let url = ruler_api_url.clone() + "api/v1/rules/"
        + &namespace.clone() + "/" + &rule_group.name;
    debug!("ruler URL is {}, going to delete group", url);
    let response = client.delete(&url)
        .header("X-Scope-OrgID", tenant_id)
        .send()
        .await;
    check_response_202(response).await;
}

// create or update resource in k8s
pub async fn create_or_update_k8s_resource(
    k8s_client: Client,
    tenant_id: &String,
    namespace: &String,
    rule_group: kube_lib::GroupSpec
) {
    let cli = k8s_client.clone();
    match kube_lib::discover_open_metrics_rules(cli.clone(), namespace).await {
        Ok(rule_vec) => {
            let mut found: Option<kube_lib::OpenMetricsRule> = None;
            let rule_group_name = rule_group.name.clone();
            for mut rule in rule_vec.iter().cloned().into_iter() {
                if rule.spec.tenants.contains(tenant_id) {
                    let mut groups_with_idx = Vec::new();
                    for g in rule.spec.groups.iter().cloned().into_iter() {
                        groups_with_idx.push((g, -1));
                    };
                    let (idx, group_named, _k8s_idx) =
                        rules::find_group_named(&groups_with_idx, &rule_group.name);
                    if group_named.is_some() {
                        rule.spec.groups.insert(idx as usize, rule_group.clone());
                        found = Some(rule);
                        break;
                    };
                };
            };
            let not_found = found.clone().is_none();
            let api : Api<kube_lib::OpenMetricsRule> = Api::namespaced(
                cli.clone(), namespace);

            let resource_name = if not_found {
                String::from("om-mt-k-ruler-src-") + &rule_group_name.replace("_", "-")
            } else {
                // It is safe to unwrap, since we just found this rule via discover_open_metrics()
                let rule = found.clone().unwrap();
                rule.metadata.name.unwrap()
            };

            let open_metrics_rule = if not_found {
                // Group not exist before.
                 kube_lib::OpenMetricsRule::new(
                     &resource_name,
                     kube_lib::OpenMetricsRuleSpec {
                         tenants: vec![tenant_id.clone()],
                         description: Some(String::from("open-metrics-multi-tenancy-kit-sourced-rule")),
                         groups: vec![rule_group.clone()]
                     }
                 )
            } else {
                found.clone().unwrap()
            };
            // Mark recent status update.
            // Since group was sourced from ruler, it is safe to assume
            // it is identical.
            resource_updated(&api, &resource_name, open_metrics_rule).await;
        },
        Err(msg) => {
            error!("failed to discover current rule set: {}", msg);
        }
    };
}


// renew resource status
pub async fn resource_updated(api: &Api<kube_lib::OpenMetricsRule>, resource_name: &String, mut open_metrics_rule: kube_lib::OpenMetricsRule) {
    let status = kube_lib::OpenMetricsRuleStatus {
        ruler_updated: true
    };
    open_metrics_rule.status = Some(status);
    let ssapply = PatchParams::apply("openmetricsrule").force();
    match api.patch(
        &resource_name.clone(),
        &ssapply,
        &Patch::Apply(&open_metrics_rule)
    ).await {
        Ok(resp) => {
            // safe to unwrap since yaml received from k8s
            info!("patched k8s resource: {}", serde_yaml::to_string(&resp).unwrap());
        },
        Err(e) => {
            error!("failed to apply rule: {} because of {}", e, &resource_name);
        }
    };
}