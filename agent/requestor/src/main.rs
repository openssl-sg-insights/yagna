use actix_rt::Arbiter;
use chrono::Utc;
use futures::{channel::mpsc, prelude::*};
use std::{path::PathBuf, time::Duration};
use structopt::StructOpt;
use url::Url;

use ya_client::payment::requestor::RequestorApi as PaymentApi;
use ya_client::{
    activity::ActivityRequestorApi, market::MarketRequestorApi, web::WebClient, web::WebInterface,
};
use ya_model::{
    activity::{
        activity_state::{State as ActivityState, StatePair},
        ExeScriptRequest,
    },
    market::{
        proposal::State as ProposalState, AgreementProposal, Demand, Proposal, RequestorEvent,
    },
    payment::{Acceptance, EventType, NewAllocation},
};

#[derive(StructOpt)]
struct AppSettings {
    /// Authorization token to server
    #[structopt(long = "app-key", env = "YAGNA_APPKEY", hide_env_values = true)]
    app_key: String,

    /// Market API URL
    #[structopt(long = "market-url", env = MarketRequestorApi::API_URL_ENV_VAR)]
    market_url: Url,

    /// Activity API URL
    #[structopt(long = "activity-url", env = ActivityRequestorApi::API_URL_ENV_VAR)]
    activity_url: Option<Url>,

    #[structopt(long = "payment-url", env = "YAGNA_PAYMENT_URL")]
    payment_url: Option<Url>,

    #[structopt(long = "exe-script")]
    exe_script: PathBuf,
}

impl AppSettings {
    fn market_api(&self) -> Result<ya_client::market::MarketRequestorApi, anyhow::Error> {
        Ok(WebClient::with_token(&self.app_key)?.interface_at(self.market_url.clone()))
    }

    fn activity_api(&self) -> Result<ActivityRequestorApi, anyhow::Error> {
        let client = WebClient::with_token(&self.app_key)?;
        if let Some(url) = &self.activity_url {
            Ok(client.interface_at(url.clone()))
        } else {
            Ok(client.interface()?)
        }
    }

    fn payment_api(&self) -> Result<PaymentApi, anyhow::Error> {
        let client = WebClient::with_token(&self.app_key)?;
        if let Some(url) = &self.payment_url {
            Ok(client.interface_at(url.clone()))
        } else {
            Ok(client.interface()?)
        }
    }
}

enum ProcessOfferResult {
    ProposalId(String),
    AgreementId(String),
}

async fn process_offer(
    requestor_api: MarketRequestorApi,
    offer: Proposal,
    subscription_id: &str,
    my_demand: Demand,
) -> Result<ProcessOfferResult, anyhow::Error> {
    let proposal_id = offer.proposal_id()?.clone();

    if offer.state.unwrap_or(ProposalState::Initial) == ProposalState::Initial {
        if offer.prev_proposal_id.is_some() {
            anyhow::bail!("Proposal in Initial state but with prev id: {:#?}", offer)
        }
        let bespoke_proposal = offer.counter_demand(my_demand)?;
        let new_proposal_id = requestor_api
            .counter_proposal(&bespoke_proposal, subscription_id)
            .await?;
        return Ok(ProcessOfferResult::ProposalId(new_proposal_id));
    }

    let new_agreement_id = proposal_id;
    let new_agreement = AgreementProposal::new(
        new_agreement_id.clone(),
        Utc::now() + chrono::Duration::hours(2),
    );
    let _ack = requestor_api.create_agreement(&new_agreement).await?;
    log::info!("confirm agreement = {}", new_agreement_id);
    requestor_api.confirm_agreement(&new_agreement_id).await?;
    log::info!("wait for agreement = {}", new_agreement_id);
    requestor_api
        .wait_for_approval(&new_agreement_id, Some(7.879))
        .await?;
    log::info!("agreement = {} CONFIRMED!", new_agreement_id);

    Ok(ProcessOfferResult::AgreementId(new_agreement_id))
}

async fn spawn_workers(
    requestor_api: MarketRequestorApi,
    subscription_id: &str,
    my_demand: &Demand,
    tx: futures::channel::mpsc::Sender<String>,
) -> Result<(), anyhow::Error> {
    loop {
        let events = requestor_api
            .collect(&subscription_id, Some(2.0), Some(5))
            .await?;

        if !events.is_empty() {
            log::debug!("market events={:#?}", events);
        } else {
            tokio::time::delay_for(Duration::from_millis(3000)).await;
        }
        for event in events {
            match event {
                RequestorEvent::ProposalEvent {
                    event_date: _,
                    proposal,
                } => {
                    let mut tx = tx.clone();
                    let requestor_api = requestor_api.clone();
                    let my_subs_id = subscription_id.to_string();
                    let my_demand = my_demand.clone();
                    Arbiter::spawn(async move {
                        match process_offer(requestor_api, proposal, &my_subs_id, my_demand).await {
                            Ok(ProcessOfferResult::ProposalId(id)) => {
                                log::info!("responded with counter proposal (id: {})", id)
                            }
                            Ok(ProcessOfferResult::AgreementId(id)) => tx.send(id).await.unwrap(),
                            Err(e) => {
                                log::error!("unable to process offer: {}", e);
                                return;
                            }
                        }
                    });
                }
                _ => {
                    log::warn!("invalid response");
                }
            }
        }
    }
}

fn build_demand(node_name: &str) -> Demand {
    Demand {
        properties: serde_json::json!({
            "golem": {
                "node": {
                    "id": {
                        "name": node_name
                    },
                    "ala": 1
                },
                "srv": {
                    "comp":{
                        "wasm": {
                            "task_package": "http://localhost:8000/rust-wasi-tutorial.zip"
                        }
                    }
                }
            }
        }),
        constraints: r#"(&
            (golem.inf.mem.gib>0.5)
            (golem.inf.storage.gib>1)
            (golem.com.pricing.model=linear)
        )"#
        .to_string(),

        demand_id: Default::default(),
        requestor_id: Default::default(),
    }
}

async fn process_agreement(
    activity_api: &ActivityRequestorApi,
    agreement_id: String,
    exe_script: &PathBuf,
) -> Result<(), anyhow::Error> {
    log::info!("GOT new agreement = {}", agreement_id);

    let act_id = activity_api
        .control()
        .create_activity(&agreement_id)
        .await?;
    log::info!("GOT new activity = (({})); YAY!", act_id);

    let contents = std::fs::read_to_string(&exe_script)?;
    let commands_cnt = match serde_json::from_str(&contents)? {
        serde_json::Value::Array(arr) => {
            log::info!("script commands cnt: {}", arr.len());
            arr.len()
        }
        _ => 0,
    };

    let batch_id = activity_api
        .control()
        .exec(ExeScriptRequest::new(contents), &act_id)
        .await?;
    log::info!("got batch_id={}", batch_id);

    loop {
        let state = activity_api.state().get_state(&act_id).await?;
        if state.state == StatePair::from(ActivityState::Terminated) {
            log::info!("activity {} terminated: {:?}", act_id, state);
            break;
        }

        log::info!("activity {} state: {:?}", act_id, state);
        let results = activity_api
            .control()
            .get_exec_batch_results(&act_id, &batch_id, Some(7))
            .await?;

        log::info!("batch results {:?}", results);

        if results.len() >= commands_cnt {
            break;
        }

        tokio::time::delay_for(Duration::from_millis(700)).await;
    }

    tokio::time::delay_for(Duration::from_millis(7000)).await;

    log::info!("destroying activity = (({})); AGRRR!", act_id);
    activity_api.control().destroy_activity(&act_id).await?;
    log::info!("I'M DONE FOR NOW");

    Ok(())
}

#[actix_rt::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();
    env_logger::init();
    let started_at = Utc::now();
    let settings = AppSettings::from_args();

    let payment_api = settings.payment_api()?;

    let node_name = "test1";

    let my_demand = build_demand(node_name);
    //(golem.runtime.wasm.wasi.version@v=*)

    let allocation = NewAllocation {
        total_amount: 10.into(),
        timeout: None,
        make_deposit: false,
    };
    let new_allocation = payment_api.create_allocation(&allocation).await.unwrap();

    log::info!("Allocated {} GNT.", &allocation.total_amount);

    let market_api = settings.market_api()?;
    let subscription_id = market_api.subscribe(&my_demand).await?;

    log::info!("sub_id={}", subscription_id);

    let mkt_api = market_api.clone();
    let sub_id = subscription_id.clone();
    Arbiter::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        mkt_api.unsubscribe(&sub_id).await.unwrap();
    });

    let mkt_api = market_api.clone();
    let sub_id = subscription_id.clone();
    let (tx, mut rx) = mpsc::channel::<String>(1);
    Arbiter::spawn(async move {
        if let Err(e) = spawn_workers(mkt_api, &sub_id, &my_demand, tx).await {
            log::error!("spawning workers for {} error: {}", sub_id, e);
        }
    });

    // log incoming debit notes
    {
        let payment_api = payment_api.clone();
        let mut ts = started_at.clone();
        Arbiter::spawn(async move {
            loop {
                let next_ts = Utc::now();
                let events = match payment_api.get_debit_note_events(Some(&ts)).await {
                    Err(e) => {
                        log::error!("fail get debit notes events: {}", e);
                        break;
                    }
                    Ok(events) => events,
                };

                for event in events {
                    log::info!("got debit note event {:?}", event);
                }
                ts = next_ts;
                tokio::time::delay_for(Duration::from_secs(15)).await;
            }
        })
    }

    let activity_api = settings.activity_api()?;
    let exe_script = settings.exe_script.clone();
    Arbiter::spawn(async move {
        while let Some(id) = rx.next().await {
            if let Err(e) = process_agreement(&activity_api, id.clone(), &exe_script).await {
                log::error!("processing agreement id {} error: {}", id, e);
            }
            // TODO: Market doesn't support agreement termination yet.
            let terminate_result = market_api.terminate_agreement(&id).await;
            log::info!("agreement: {}, terminated: {:?}", id, terminate_result);
        }
    });

    Arbiter::spawn(async move {
        let mut ts = started_at;

        loop {
            let next_ts = Utc::now();

            let events = payment_api.get_invoice_events(Some(&ts)).await.unwrap();
            // TODO: timeout on get_invoice_events does not work
            if events.is_empty() {
                tokio::time::delay_for(Duration::from_secs(15)).await;
            }

            for event in events {
                match event.event_type {
                    EventType::Received => {
                        let invoice = payment_api.get_invoice(&event.invoice_id).await.unwrap();
                        let acceptance = Acceptance {
                            total_amount_accepted: invoice.amount,
                            allocation_id: new_allocation.allocation_id.clone(),
                        };
                        let result = payment_api
                            .accept_invoice(&event.invoice_id, &acceptance)
                            .await;
                        log::info!("payment acceptance result: {:?}", result);
                    }
                    _ => (),
                }
                ts = next_ts;
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    settings.market_api()?.unsubscribe(&subscription_id).await?;
    Ok(())
}
