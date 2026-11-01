use std::collections::{HashMap, VecDeque};

use tokio::sync::{mpsc, oneshot};

use crate::{
    ai::{self, AiError, StreamCallback},
    models::{Provider, TranslateRequest, TranslationResult},
    store::AppStore,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    High,
    Low,
}

#[derive(Clone)]
pub struct AiScheduler {
    sender: mpsc::UnboundedSender<Command>,
}

enum Command {
    Translate {
        request: TranslateRequest,
        response: oneshot::Sender<Result<TranslationResult, AiError>>,
        priority: Priority,
        stream: Option<StreamCallback>,
    },
    TranslateBatch {
        request: TranslateRequest,
        texts: Vec<String>,
        response: oneshot::Sender<Result<Vec<String>, AiError>>,
        priority: Priority,
        stream: Option<StreamCallback>,
    },
    Summarize {
        request: TranslateRequest,
        text: String,
        response: oneshot::Sender<Result<String, AiError>>,
        priority: Priority,
    },
    TranslateImage {
        request: TranslateRequest,
        filename: String,
        image: Vec<u8>,
        response: oneshot::Sender<Result<Vec<u8>, AiError>>,
        priority: Priority,
    },
}

enum Work {
    Translate {
        request: TranslateRequest,
        response: oneshot::Sender<Result<TranslationResult, AiError>>,
        stream: Option<StreamCallback>,
    },
    TranslateBatch {
        request: TranslateRequest,
        texts: Vec<String>,
        response: oneshot::Sender<Result<Vec<String>, AiError>>,
        stream: Option<StreamCallback>,
    },
    Summarize {
        request: TranslateRequest,
        text: String,
        response: oneshot::Sender<Result<String, AiError>>,
    },
    TranslateImage {
        request: TranslateRequest,
        filename: String,
        image: Vec<u8>,
        response: oneshot::Sender<Result<Vec<u8>, AiError>>,
    },
}

impl Work {
    fn provider_id(&self) -> &str {
        match self {
            Self::Translate { request, .. }
            | Self::TranslateBatch { request, .. }
            | Self::Summarize { request, .. }
            | Self::TranslateImage { request, .. } => &request.provider_id,
        }
    }
}

impl AiScheduler {
    pub fn new(store: AppStore) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_scheduler(store, receiver));
        Self { sender }
    }

    pub async fn translate(
        &self,
        request: TranslateRequest,
        priority: Priority,
    ) -> Result<TranslationResult, AiError> {
        self.translate_with_stream(request, priority, None).await
    }

    pub async fn translate_with_stream(
        &self,
        request: TranslateRequest,
        priority: Priority,
        stream: Option<StreamCallback>,
    ) -> Result<TranslationResult, AiError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::Translate {
                request,
                response,
                priority,
                stream,
            })
            .map_err(|_| AiError::Message("AI scheduler is unavailable".to_string()))?;
        receiver
            .await
            .map_err(|_| AiError::Message("AI scheduler stopped unexpectedly".to_string()))?
    }

    pub async fn translate_batch(
        &self,
        request: TranslateRequest,
        texts: Vec<String>,
        priority: Priority,
        stream: Option<StreamCallback>,
    ) -> Result<Vec<String>, AiError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::TranslateBatch {
                request,
                texts,
                response,
                priority,
                stream,
            })
            .map_err(|_| AiError::Message("AI scheduler is unavailable".to_string()))?;
        receiver
            .await
            .map_err(|_| AiError::Message("AI scheduler stopped unexpectedly".to_string()))?
    }

    pub async fn summarize(
        &self,
        request: TranslateRequest,
        text: String,
        priority: Priority,
    ) -> Result<String, AiError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::Summarize {
                request,
                text,
                response,
                priority,
            })
            .map_err(|_| AiError::Message("AI scheduler is unavailable".to_string()))?;
        receiver
            .await
            .map_err(|_| AiError::Message("AI scheduler stopped unexpectedly".to_string()))?
    }

    pub async fn translate_image(
        &self,
        request: TranslateRequest,
        filename: String,
        image: Vec<u8>,
        priority: Priority,
    ) -> Result<Vec<u8>, AiError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Command::TranslateImage {
                request,
                filename,
                image,
                response,
                priority,
            })
            .map_err(|_| AiError::Message("AI scheduler is unavailable".to_string()))?;
        receiver
            .await
            .map_err(|_| AiError::Message("AI scheduler stopped unexpectedly".to_string()))?
    }
}

struct Completion {
    provider_id: String,
    priority: Priority,
    work: Work,
    result: WorkResult,
}

enum WorkResult {
    Cancelled,
    Translate(Result<TranslationResult, AiError>),
    TranslateBatch(Result<Vec<String>, AiError>),
    Summarize(Result<String, AiError>),
    TranslateImage(Result<Vec<u8>, AiError>),
}

struct PendingWork {
    provider_id: String,
    priority: Priority,
    work: Work,
}

#[derive(Default)]
struct ActiveCounts {
    high: usize,
    low: usize,
}

async fn run_scheduler(store: AppStore, mut receiver: mpsc::UnboundedReceiver<Command>) {
    let (completion_sender, mut completion_receiver) = mpsc::unbounded_channel();
    let mut high: VecDeque<PendingWork> = VecDeque::new();
    let mut low: VecDeque<PendingWork> = VecDeque::new();
    let mut active: HashMap<String, ActiveCounts> = HashMap::new();

    loop {
        while let Some(pending) = take_runnable(&store, &mut high, &active, Priority::High)
            .or_else(|| take_runnable(&store, &mut low, &active, Priority::Low))
        {
            let provider_id = pending.provider_id.clone();
            let priority = pending.priority;
            let counts = active.entry(provider_id.clone()).or_default();
            match priority {
                Priority::High => counts.high += 1,
                Priority::Low => counts.low += 1,
            }
            let store = store.clone();
            let completion_sender = completion_sender.clone();
            tokio::spawn(async move {
                let completion = execute_work(store, provider_id, priority, pending.work).await;
                let _ = completion_sender.send(completion);
            });
        }

        if high.is_empty() && low.is_empty() && active.is_empty() {
            let Some(command) = receiver.recv().await else {
                return;
            };
            enqueue(command, &mut high, &mut low);
            continue;
        }

        tokio::select! {
            command = receiver.recv() => {
                if let Some(command) = command {
                    enqueue(command, &mut high, &mut low);
                } else {
                    return;
                }
            }
            completion = completion_receiver.recv() => {
                if let Some(completion) = completion {
                    if let Some(counts) = active.get_mut(&completion.provider_id) {
                        match completion.priority {
                            Priority::High => counts.high = counts.high.saturating_sub(1),
                            Priority::Low => counts.low = counts.low.saturating_sub(1),
                        }
                        if counts.high == 0 && counts.low == 0 {
                            active.remove(&completion.provider_id);
                        }
                    }
                    send_completion(completion);
                }
            }
        }
    }
}

fn enqueue(command: Command, high: &mut VecDeque<PendingWork>, low: &mut VecDeque<PendingWork>) {
    let (priority, work) = match command {
        Command::Translate {
            request,
            response,
            priority,
            stream,
        } => (
            priority,
            Work::Translate {
                request,
                response,
                stream,
            },
        ),
        Command::TranslateBatch {
            request,
            texts,
            response,
            priority,
            stream,
        } => (
            priority,
            Work::TranslateBatch {
                request,
                texts,
                response,
                stream,
            },
        ),
        Command::Summarize {
            request,
            text,
            response,
            priority,
        } => (
            priority,
            Work::Summarize {
                request,
                text,
                response,
            },
        ),
        Command::TranslateImage {
            request,
            filename,
            image,
            response,
            priority,
        } => (
            priority,
            Work::TranslateImage {
                request,
                filename,
                image,
                response,
            },
        ),
    };
    let pending = PendingWork {
        provider_id: work.provider_id().to_string(),
        priority,
        work,
    };
    match priority {
        Priority::High => high.push_back(pending),
        Priority::Low => low.push_back(pending),
    }
}

fn take_runnable(
    store: &AppStore,
    queue: &mut VecDeque<PendingWork>,
    active: &HashMap<String, ActiveCounts>,
    priority: Priority,
) -> Option<PendingWork> {
    let position = queue.iter().position(|pending| {
        let limit = provider_limit(store, &pending.provider_id, priority);
        let counts = active.get(&pending.provider_id);
        let current = counts.map(|value| value.high + value.low).unwrap_or(0);
        current < limit
    })?;
    queue.remove(position)
}

fn provider_limit(store: &AppStore, provider_id: &str, priority: Priority) -> usize {
    let provider = store.provider(provider_id).unwrap_or(Provider {
        id: provider_id.to_string(),
        name: String::new(),
        base_url: String::new(),
        model: String::new(),
        reasoning_parser: Default::default(),
        api_key: String::new(),
        proxy_mode: Default::default(),
        enabled: false,
        supports_images: false,
        context_size: 32_768,
        max_segments: 16,
        max_concurrent: 2,
        text_translation_model: false,
    });
    let max = provider.max_concurrent.max(1);
    if matches!(priority, Priority::Low) && provider.text_translation_model && max > 1 {
        max - 1
    } else {
        max
    }
}

async fn execute_work(
    store: AppStore,
    provider_id: String,
    priority: Priority,
    mut work: Work,
) -> Completion {
    // Dropping a cancelled caller also aborts its provider request and releases its slot.
    let result = match &mut work {
        Work::Translate {
            request,
            stream,
            response,
        } => tokio::select! {
            biased;
            _ = response.closed() => WorkResult::Cancelled,
            result = ai::translate(&store, request, stream.clone()) => WorkResult::Translate(result),
        },
        Work::TranslateBatch {
            request,
            texts,
            stream,
            response,
        } => tokio::select! {
            biased;
            _ = response.closed() => WorkResult::Cancelled,
            result = ai::translate_batch(&store, request, texts, stream.clone()) => WorkResult::TranslateBatch(result),
        },
        Work::Summarize {
            request,
            text,
            response,
        } => tokio::select! {
            biased;
            _ = response.closed() => WorkResult::Cancelled,
            result = ai::summarize(&store, request, text) => WorkResult::Summarize(result),
        },
        Work::TranslateImage {
            request,
            filename,
            image,
            response,
        } => tokio::select! {
            biased;
            _ = response.closed() => WorkResult::Cancelled,
            result = ai::translate_image(&store, request, filename, image.clone()) => WorkResult::TranslateImage(result),
        },
    };
    Completion {
        provider_id,
        priority,
        work,
        result,
    }
}

fn send_completion(completion: Completion) {
    match (completion.work, completion.result) {
        (Work::Translate { response, .. }, WorkResult::Translate(result)) => {
            let _ = response.send(result);
        }
        (Work::TranslateBatch { response, .. }, WorkResult::TranslateBatch(result)) => {
            let _ = response.send(result);
        }
        (Work::Summarize { response, .. }, WorkResult::Summarize(result)) => {
            let _ = response.send(result);
        }
        (Work::TranslateImage { response, .. }, WorkResult::TranslateImage(result)) => {
            let _ = response.send(result);
        }
        _ => {}
    }
}
