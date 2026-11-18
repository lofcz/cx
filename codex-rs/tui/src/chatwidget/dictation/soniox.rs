//! Upload one completed recording, retrieve its final transcript, then delete both resources.
use anyhow::{Context, Result, bail, ensure};
use codex_http_client::{ClientRouteClass, HttpClientFactory, RequestBuilder};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
struct File {
    id: Uuid,
}
#[derive(Deserialize)]
struct Job {
    id: Uuid,
    status: String,
    error_message: Option<String>,
}
#[derive(Deserialize)]
struct Transcript {
    text: String,
}

async fn json<T: DeserializeOwned>(request: RequestBuilder, key: &str) -> Result<T> {
    Ok(request
        .bearer_auth(key)
        .timeout(Duration::from_secs(45))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub(super) async fn transcribe(
    factory: HttpClientFactory,
    wav: Vec<u8>,
    model: String,
    key: String,
) -> Result<String> {
    run(factory, "https://api.soniox.com/v1", wav, model, key).await
}

async fn run(
    factory: HttpClientFactory,
    api: &str,
    wav: Vec<u8>,
    model: String,
    key: String,
) -> Result<String> {
    let client = factory.build_client_without_request_logging(api, ClientRouteClass::Other)?;
    let boundary = Uuid::new_v4();
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\n").into_bytes();
    body.extend(wav);
    body.extend(format!("\r\n--{boundary}--\r\n").bytes());
    let upload = client
        .post(format!("{api}/files"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body);
    let file: File = json(upload, &key)
        .await
        .context("Soniox audio upload failed")?;
    let mut job_url = None;
    let result = async {
        let create = client
            .post(format!("{api}/transcriptions"))
            .json(&serde_json::json!({"model": model, "file_id": file.id}));
        let mut job: Job = json(create, &key)
            .await
            .context("Could not create Soniox transcription")?;
        let url = format!("{api}/transcriptions/{}", job.id);
        job_url = Some(url.clone());
        tokio::time::timeout(Duration::from_secs(300), async {
            while matches!(job.status.as_str(), "queued" | "processing") {
                tokio::time::sleep(Duration::from_secs(1)).await;
                job = json(client.get(&url), &key).await?;
            }
            if job.status != "completed" {
                bail!(
                    "Soniox transcription failed: {}",
                    job.error_message.unwrap_or(job.status)
                );
            }
            let transcript: Transcript =
                json(client.get(format!("{url}/transcript")), &key).await?;
            ensure!(
                !transcript.text.trim().is_empty(),
                "Soniox detected no speech."
            );
            Ok(transcript.text)
        })
        .await
        .context("Soniox transcription timed out")?
    }
    .await;
    for url in job_url
        .into_iter()
        .chain([format!("{api}/files/{}", file.id)])
    {
        let deleted = client
            .delete(url)
            .bearer_auth(&key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .and_then(|r| r.error_for_status());
        if let Err(err) = deleted {
            tracing::warn!("Could not remove dictation resource from Soniox: {err}");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn async_job_uses_configured_model_and_deletes_both_resources() {
        let server = MockServer::start().await;
        let file = Uuid::new_v4();
        let job = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path("/files"))
            .and(header("Authorization", "Bearer test"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id":file})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/transcriptions"))
            .and(body_json(
                serde_json::json!({"model":"custom-model","file_id":file}),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"id":job,"status":"queued"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let job_path = format!("/transcriptions/{job}");
        Mock::given(method("GET"))
            .and(path(&*job_path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id":job,"status":"completed"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{job_path}/transcript")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text":"dictated"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        for resource in [job_path, format!("/files/{file}")] {
            Mock::given(method("DELETE"))
                .and(path(resource))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        assert_eq!(
            run(
                HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault),
                &server.uri(),
                vec![1, 2],
                "custom-model".into(),
                "test".into()
            )
            .await
            .unwrap(),
            "dictated"
        );
    }
}
