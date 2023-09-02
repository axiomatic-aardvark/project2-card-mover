#[macro_use]
extern crate log;
extern crate reqwest;
extern crate serde_json;

use octocrab::models;
use serde_json::Value;
use std::env;
use warp::Filter;

#[tokio::main]
async fn main() {
    env_logger::init();

    info!("Starting server...");

    let webhook = warp::path!("webhook")
        .and(warp::post())
        .and(warp::body::json::<serde_json::Value>())
        .and_then(handle_webhook);

    let routes = webhook;

    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}

async fn prepare_graphql_query(node_id: &str) -> String {
    let query_template = r#"
    query {
        node(id: "$nodeId") {
            ... on ProjectV2Item {
                id
                fieldValues(first: 8) {
                    nodes {
                        ... on ProjectV2ItemFieldTextValue {
                            text
                            field {
                                ... on ProjectV2FieldCommon {
                                    name
                                }
                            }
                        }
                        ... on ProjectV2ItemFieldDateValue {
                            date
                            field {
                                ... on ProjectV2FieldCommon {
                                    name
                                }
                            }
                        }
                        ... on ProjectV2ItemFieldSingleSelectValue {
                            name
                            field {
                                ... on ProjectV2FieldCommon {
                                    name
                                }
                            }
                        }
                    }
                }
                content {
                    ... on Issue {
                        id
                        title
                        repository {
                            name
                            owner {
                                login
                            }
                        }
                        assignees(first: 10) {
                            nodes {
                                login
                            }
                        }
                    }
                    ... on PullRequest {
                        id
                        title
                        assignees(first: 10) {
                            nodes {
                                login
                            }
                        }
                    }
                }
            }
        }
    }
    "#;
    query_template.replace("$nodeId", node_id)
}

async fn prepare_issue_number_query(issue_id: &str) -> String {
    let query_template = r#"
    query {
        node(id: "$issueId") {
            ... on Issue {
                number
            }
        }
    }
    "#;
    query_template.replace("$issueId", issue_id)
}

async fn send_graphql_request(query: &str, github_token: String) -> Result<Value, reqwest::Error> {
    let client = reqwest::Client::new();
    let query_object = serde_json::json!({ "query": query });

    let response = client
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {}", github_token))
        .header("User-Agent", "ProjectCardMover")
        .json(&query_object)
        .send()
        .await?;

    let json_response: Value = response.json().await?;
    Ok(json_response)
}

async fn handle_webhook(payload: Value) -> Result<impl warp::Reply, warp::Rejection> {
    info!("Received a webhook call with payload: {:?}", payload);
    let github_token = env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN must be set");

    if let Some(action) = payload.get("action") {
        if action == "reordered" {
            info!("It's a reordered event.");

            if let Some(node_id) = payload
                .get("projects_v2_item")
                .and_then(|item| item.get("node_id"))
            {
                let node_id_str = node_id.as_str().unwrap_or_default();
                let query = prepare_graphql_query(node_id_str).await;

                match send_graphql_request(&query, github_token.clone()).await {
                    Ok(json) => {
                        info!("Received GraphQL response: {:?}", json);

                        let mut is_done = false;
                        if let Some(field_values) =
                            json["data"]["node"]["fieldValues"]["nodes"].as_array()
                        {
                            for field_value in field_values {
                                if field_value["field"]["name"].as_str() == Some("Status")
                                    && field_value["name"].as_str() == Some("Done")
                                {
                                    is_done = true;
                                    break;
                                }
                            }
                        }

                        if is_done {
                            info!("Status is Done.");

                            if let Some(issue_id) = json["data"]["node"]["content"]["id"].as_str() {
                                let issue_number_query = prepare_issue_number_query(issue_id).await;

                                match send_graphql_request(
                                    &issue_number_query,
                                    github_token.clone(),
                                )
                                .await
                                {
                                    Ok(issue_json) => {
                                        if let Some(issue_number) =
                                            issue_json["data"]["node"]["number"].as_u64()
                                        {
                                            info!("The issue number that this card is representing is: {}", issue_number);

                                            if let Some(repo_name) = json["data"]["node"]["content"]
                                                ["repository"]["name"]
                                                .as_str()
                                            {
                                                if let Some(repo_owner) = json["data"]["node"]
                                                    ["content"]["repository"]["owner"]["login"]
                                                    .as_str()
                                                {
                                                    info!(
                                                        "The issue is in repository: {}/{}",
                                                        repo_owner, repo_name
                                                    );

                                                    let octocrab = octocrab::Octocrab::builder()
                                                        .personal_token(github_token)
                                                        .build()
                                                        .unwrap();

                                                    let _ = octocrab
                                                        .issues(repo_owner, repo_name)
                                                        .update(issue_number)
                                                        .state(models::IssueState::Closed)
                                                        // Send the request
                                                        .send()
                                                        .await
                                                        .unwrap();
                                                } else {
                                                    info!("Repository owner not found.");
                                                }
                                            } else {
                                                info!("Repository information not found.");
                                            }
                                        } else {
                                            info!("Issue number not found in the second query.");
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to make the second GraphQL request: {}", e);
                                    }
                                }
                            } else {
                                info!("Issue ID not found in the first query.");
                            }
                        } else {
                            info!("Status is not Done. Ignoring.");
                        }
                    }
                    Err(e) => {
                        error!("Failed to make GraphQL request: {}", e);
                    }
                }
            } else {
                info!("Node ID not found in payload.");
            }
        } else {
            info!("Not a reordered event. Ignoring.");
        }
    } else {
        info!("Action field not found in payload.");
    }

    Ok("Webhook received")
}
