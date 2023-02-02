use std::path::PathBuf;

use anyhow::Result;
use log::*;
use futures::Stream;
use futures::stream::StreamExt;
use async_stream::stream;
use tonic::{Request, Status};
use tonic::codegen::InterceptedService;
use tonic::service::Interceptor;
use tonic::transport::{Channel, Uri};
use tonic::metadata::AsciiMetadataValue;

pub mod pb {
    tonic::include_proto!("edgebit.v1alpha.enrollment");
    tonic::include_proto!("edgebit.v1alpha.inventory");
}

use pb::enrollment_service_client::EnrollmentServiceClient;
use pb::inventory_service_client::InventoryServiceClient;

use edgebit_agent::packages::PkgRef;

const TOKEN_FILE: &str = "/var/lib/edgebit/token";
struct AuthInterceptor {
    auth_val: AsciiMetadataValue,
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> std::result::Result<Request<()>, Status> {
        request.metadata_mut().insert("authorization", self.auth_val.clone());
        Ok(request)
    }
}

pub struct Client {
    inner: InventoryServiceClient<InterceptedService<Channel, AuthInterceptor>>,
}

impl Client {
    pub async fn connect(endpoint: Uri, deploy_token: String) -> Result<Self> {
        let channel = Channel::builder(endpoint)
            .connect()
            .await?;

        let token = match load_token() {
            Ok(token) => token,
            Err(_) => enroll(channel.clone(), deploy_token).await?,
        };

        let auth_val: AsciiMetadataValue = format!("Bearer {token}").parse()?;
        let auth_interceptor = AuthInterceptor{auth_val};

        let inner = InventoryServiceClient::with_interceptor(channel, auth_interceptor);

        Ok(Self{inner})
    }

    pub async fn upload_sbom(&mut self, sbom_doc: String) -> Result<()> {
        // Header first
        let header_req = pb::UploadSbomRequest{
            kind: Some(pb::upload_sbom_request::Kind::Header(
                pb::UploadSbomHeader{
                    format: pb::SbomFormat::SbomfmtSyft as i32,
                },
            )),
        };

        let header_stream = futures::stream::once(
            futures::future::ready(header_req)
        );

        let stream = header_stream.chain(data_stream(sbom_doc.into_bytes()));

        self.inner.upload_sbom(stream).await?;

        Ok(())
    }

    pub async fn report_in_use(&mut self, pkgs: Vec<PkgRef>) -> Result<()> {
        let in_use = pkgs.into_iter()
            .map(|p| {
                pb::PkgInUse{
                    id: p.id,
                    files: p.filenames,
                }
            })
            .collect();

        let req = pb::ReportInUseRequest{
            in_use,
        };

        self.inner.report_in_use(req).await?;
        Ok(())
    }

}

fn load_token() -> Result<String> {
    Ok(std::fs::read_to_string(TOKEN_FILE)?)
}

fn save_token(token: &str) -> Result<()> {
    let token_file = PathBuf::from(TOKEN_FILE);
    let dir = token_file.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    Ok(std::fs::write(TOKEN_FILE, token)?)
}

fn hostname() -> String {
    gethostname::gethostname().into_string()
        .unwrap_or_else(|s| {
            warn!("Hostname contains invalid UTF-8");
            s.to_string_lossy().into_owned()
        })
}

async fn enroll(channel: Channel, deploy_token: String) -> Result<String> {
    let mut enroll_svc = EnrollmentServiceClient::new(channel);

    let req = pb::EnrollAgentRequest{
        deployment_token: deploy_token,
        hostname: hostname(),
    };

    let resp = enroll_svc.enroll_agent(req)
        .await?
        .into_inner();

    save_token(&resp.agent_token)
        .unwrap_or_else(|err| {
            error!("Error saving agent token: {err}");
        });

    Ok(resp.agent_token)
}

fn data_stream(buf: Vec<u8>) -> impl Stream<Item=pb::UploadSbomRequest> {
    stream!{
        for chunk in buf.chunks(64*1024) {
            yield pb::UploadSbomRequest{
                kind: Some(pb::upload_sbom_request::Kind::Data(chunk.to_vec())),
            };
        }
    }
}