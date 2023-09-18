#[cfg(feature = "mkl")]
extern crate intel_mkl_src;

#[cfg(feature = "accelerate")]
extern crate accelerate_src;

use tokenizers::Tokenizer;

use crate::{Error, Generation, Query, Token};
use candle::quantized::{ggml_file, gguf_file};
use candle::{Device, Tensor};
use candle_transformers::generation::LogitsProcessor;

use candle_transformers::models::quantized_llama as model;
use model::ModelWeights;

// const DEFAULT_PROMPT: &str = "My favorite theorem is ";
//
// #[derive(Debug)]
// enum Prompt {
//     Interactive,
//     Chat,
//     One(String),
// }
//
// #[derive(Parser, Debug)]
// #[command(author, version, about, long_about = None)]
// struct Args {
//     /// GGML file to load, typically a .bin file generated by the quantize command from llama.cpp
//     #[arg(long)]
//     model: Option<String>,
//
//     /// The initial prompt, use 'interactive' for entering multiple prompts in an interactive way
//     /// and 'chat' for an interactive model where history of previous prompts and generated tokens
//     /// is preserved.
//     #[arg(long)]
//     prompt: Option<String>,
//
//     /// The length of the sample to generate (in tokens).
//     #[arg(short = 'n', long, default_value_t = 100)]
//     sample_len: usize,
//
//     /// The tokenizer config in json format.
//     #[arg(long)]
//     tokenizer: Option<String>,
//
//     /// The temperature used to generate samples, use 0 for greedy sampling.
//     #[arg(long, default_value_t = 0.8)]
//     temperature: f64,
//
//     /// Nucleus sampling probability cutoff.
//     #[arg(long)]
//     top_p: Option<f64>,
//
//     /// The seed to use when generating random samples.
//     #[arg(long, default_value_t = 299792458)]
//     seed: u64,
//
//     /// Enable tracing (generates a trace-timestamp.json file).
//     #[arg(long)]
//     tracing: bool,
//
//     /// Display the token for the specified prompt.
//     #[arg(long)]
//     verbose_prompt: bool,
//
//     /// Penalty to be applied for repeating tokens, 1. means no penalty.
//     #[arg(long, default_value_t = 1.1)]
//     repeat_penalty: f32,
//
//     /// The context size to consider for the repeat penalty.
//     #[arg(long, default_value_t = 64)]
//     repeat_last_n: usize,
//
//     /// The model size to use.
//     #[arg(long, default_value = "7b")]
//     which: Which,
//
//     /// Group-Query Attention, use 8 for the 70B version of LLaMAv2.
//     #[arg(long)]
//     gqa: Option<usize>,
// }

fn tokenizer() -> Result<Tokenizer, Error> {
    let api = hf_hub::api::sync::ApiBuilder::from_cache(super::cache()).build()?;
    let api = api.model("hf-internal-testing/llama-tokenizer".to_string());
    let tokenizer_path = api.get("tokenizer.json")?;
    Ok(Tokenizer::from_file(tokenizer_path)?)
}

fn get_model() -> Result<ModelWeights, Error> {
    let (repo, filename) = (
        "TheBloke/Llama-2-7B-Chat-GGML",
        "llama-2-7b-chat.ggmlv3.q4_0.bin",
    );
    // let (repo, filename) = (
    //     "klosax/tinyllamas-stories-gguf",
    //     "tinyllamas-stories-260k-f32.gguf",
    // );
    let api = hf_hub::api::sync::ApiBuilder::from_cache(super::cache()).build()?;
    let api = api.model(repo.to_string());
    println!("Getting {filename}");
    let model_path = api.get(filename)?;
    println!("Got {filename}");
    let start = std::time::Instant::now();
    let mut file = std::fs::File::open(&model_path)?;
    let model: ModelWeights = match model_path.extension().and_then(|v| v.to_str()) {
        Some("gguf") => {
            let model = gguf_file::Content::read(&mut file)?;
            let mut total_size_in_bytes = 0;
            for (_, tensor) in model.tensor_infos.iter() {
                let elem_count = tensor.shape.elem_count();
                total_size_in_bytes +=
                    elem_count * tensor.ggml_dtype.type_size() / tensor.ggml_dtype.blck_size();
            }
            println!(
                "loaded {:?} tensors ({}) in {:.2}s",
                model.tensor_infos.len(),
                &format_size(total_size_in_bytes),
                start.elapsed().as_secs_f32(),
            );
            ModelWeights::from_gguf(model, &mut file)?
        }
        Some("ggml" | "bin") | Some(_) | None => {
            let model = ggml_file::Content::read(&mut file)?;
            let mut total_size_in_bytes = 0;
            for (_, tensor) in model.tensors.iter() {
                let elem_count = tensor.shape().elem_count();
                total_size_in_bytes +=
                    elem_count * tensor.dtype().type_size() / tensor.dtype().blck_size();
            }
            println!(
                "loaded {:?} tensors ({}) in {:.2}s",
                model.tensors.len(),
                &format_size(total_size_in_bytes),
                start.elapsed().as_secs_f32(),
            );
            println!("params: {:?}", model.hparams);
            let default_gqa = 1;
            ModelWeights::from_ggml(model, default_gqa)?
        }
    };
    Ok(model)
}

fn print_token(next_token: u32, tokenizer: &Tokenizer) -> String {
    // Extracting the last token as a string is complicated, here we just apply some simple
    // heuristics as it seems to work well enough for this example. See the following for more
    // details:
    // https://github.com/huggingface/tokenizers/issues/1141#issuecomment-1562644141
    if let Some(text) = tokenizer.id_to_token(next_token) {
        let text = text.replace('▁', " ");
        let ascii = text
            .strip_prefix("<0x")
            .and_then(|t| t.strip_suffix('>'))
            .and_then(|t| u8::from_str_radix(t, 16).ok());
        match ascii {
            None => return text,
            Some(ascii) => {
                if let Some(chr) = char::from_u32(ascii as u32) {
                    if chr.is_ascii() {
                        return format!("{chr}");
                    }
                }
            }
        }
    }
    "".into()
}

fn format_size(size_in_bytes: usize) -> String {
    if size_in_bytes < 1_000 {
        format!("{}B", size_in_bytes)
    } else if size_in_bytes < 1_000_000 {
        format!("{:.2}KB", size_in_bytes as f64 / 1e3)
    } else if size_in_bytes < 1_000_000_000 {
        format!("{:.2}MB", size_in_bytes as f64 / 1e6)
    } else {
        format!("{:.2}GB", size_in_bytes as f64 / 1e9)
    }
}

pub struct Pipeline {
    model: ModelWeights,
    tokenizer: Tokenizer,
    query: Query,
    tokens: Vec<u32>,
    logits_processor: LogitsProcessor,
}

pub fn load_local(query: Query) -> Result<Pipeline, Error> {
    let tokenizer = tokenizer()?;
    let model = get_model()?;
    let encoded = tokenizer.encode(query.inputs.clone(), true)?;
    let tokens: Vec<u32> = encoded.get_ids().iter().cloned().collect();
    let logits_processor = LogitsProcessor::new(
        0,
        Some(query.parameters.temperature as f64),
        Some(query.parameters.top_p as f64),
    );
    Ok(Pipeline {
        model,
        tokenizer,
        query,
        logits_processor,
        tokens: tokens.to_vec(),
    })
}
pub struct PipelineIter<'a> {
    pipeline: &'a mut Pipeline,
    tokens: Vec<u32>,
    all_tokens: Vec<u32>,
    i: usize,
    last: bool,
}

impl Pipeline {
    pub fn iter(&mut self) -> PipelineIter {
        PipelineIter {
            tokens: self.tokens.clone(),
            all_tokens: self.tokens.clone(),
            pipeline: self,
            i: 0,
            last: false,
        }
    }
}

impl<'a> PipelineIter<'a> {
    fn inner_next(&mut self) -> Result<Generation, Error> {
        tracing::info!(
            "Inner next {:?} - {:?}",
            self.tokens,
            self.pipeline
                .tokenizer
                .decode(self.tokens.as_slice(), false)
        );
        let input = Tensor::new(self.tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
        let logits = self.pipeline.model.forward(&input, 0)?;
        let logits = logits.squeeze(0)?;
        let next_token = self.pipeline.logits_processor.sample(&logits)?;
        self.all_tokens.push(next_token);
        let text = print_token(next_token, &self.pipeline.tokenizer);

        self.tokens = vec![next_token];
        let generated_text = if self.i == self.pipeline.query.parameters.max_new_tokens {
            Some(self.pipeline.tokenizer.decode(&self.all_tokens, true)?)
        } else {
            None
        };
        self.i += 1;
        let generation = Generation {
            token: Token {
                id: next_token as usize,
                logprob: 0.0,
                text,
                special: false,
            },
            generated_text,
            details: None,
        };
        Ok(generation)
    }
}
impl<'a> Iterator for PipelineIter<'a> {
    type Item = Result<Generation, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.last {
            None
        } else {
            let generation = self.inner_next();
            if let Ok(generation) = &generation {
                if generation.generated_text.is_some() {
                    self.last = true;
                }
            }
            Some(generation)
        }
    }
}

// fn main() -> anyhow::Result<()> {
//     // use tracing_chrome::ChromeLayerBuilder;
//     // use tracing_subscriber::prelude::*;
//
//     // let args = Args::parse();
//     // let temperature = if args.temperature == 0. {
//     //     None
//     // } else {
//     //     Some(args.temperature)
//     // };
//     // let _guard = if args.tracing {
//     //     let (chrome_layer, guard) = ChromeLayerBuilder::new().build();
//     //     tracing_subscriber::registry().with(chrome_layer).init();
//     //     Some(guard)
//     // } else {
//     //     None
//     // };
//
//     println!(
//         "avx: {}, neon: {}, simd128: {}, f16c: {}",
//         candle::utils::with_avx(),
//         candle::utils::with_neon(),
//         candle::utils::with_simd128(),
//         candle::utils::with_f16c()
//     );
//     // println!(
//     //     "temp: {:.2} repeat-penalty: {:.2} repeat-last-n: {}",
//     //     args.temperature, args.repeat_penalty, args.repeat_last_n
//     // );
//
//
//     let mut pre_prompt_tokens = vec![];
//     loop {
//         let prompt_str = match &prompt {
//             Prompt::One(prompt) => prompt.clone(),
//             Prompt::Interactive | Prompt::Chat => {
//                 print!("> ");
//                 std::io::stdout().flush()?;
//                 let mut prompt = String::new();
//                std::io::stdin().read_line(&mut prompt)?;
//                izx
//                 if prompt.ends_with('\n') {
//                     prompt.pop();
//                     if prompt.ends_with('\r') {
//                         prompt.pop();
//                     }
//                 }
//                 prompt
//             }
//         };
//         print!("{}", &prompt_str);
//         let tokens = tokenizer
//             .encode(prompt_str, true)
//             .map_err(anyhow::Error::msg)?;
//         if args.verbose_prompt {
//             for (token, id) in tokens.get_tokens().iter().zip(tokens.get_ids().iter()) {
//                 let token = token.replace('▁', " ").replace("<0x0A>", "\n");
//                 println!("{id:7} -> '{token}'");
//             }
//         }
//
//         let prompt_tokens = [&pre_prompt_tokens, tokens.get_ids()].concat();
//         let to_sample = args.sample_len.saturating_sub(1);
//         let prompt_tokens = if prompt_tokens.len() + to_sample > model::MAX_SEQ_LEN - 10 {
//             let to_remove = prompt_tokens.len() + to_sample + 10 - model::MAX_SEQ_LEN;
//             prompt_tokens[prompt_tokens.len().saturating_sub(to_remove)..].to_vec()
//         } else {
//             prompt_tokens
//         };
//         let mut all_tokens = vec![];
//         let mut logits_processor = LogitsProcessor::new(args.seed, temperature, args.top_p);
//
//         let start_prompt_processing = std::time::Instant::now();
//         let mut next_token = {
//             let input = Tensor::new(prompt_tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
//             let logits = model.forward(&input, 0)?;
//             let logits = logits.squeeze(0)?;
//             logits_processor.sample(&logits)?
//         };
//         let prompt_dt = start_prompt_processing.elapsed();
//         all_tokens.push(next_token);
//         print_token(next_token, &tokenizer);
//
//         let start_post_prompt = std::time::Instant::now();
//         for index in 0..to_sample {
//             let input = Tensor::new(&[next_token], &Device::Cpu)?.unsqueeze(0)?;
//             let logits = model.forward(&input, prompt_tokens.len() + index)?;
//             let logits = logits.squeeze(0)?;
//             let logits = if args.repeat_penalty == 1. {
//                 logits
//             } else {
//                 let start_at = all_tokens.len().saturating_sub(args.repeat_last_n);
//                 candle_transformers::utils::apply_repeat_penalty(
//                     &logits,
//                     args.repeat_penalty,
//                     &all_tokens[start_at..],
//                 )?
//             };
//             next_token = logits_processor.sample(&logits)?;
//             all_tokens.push(next_token);
//             print_token(next_token, &tokenizer);
//         }
//         let dt = start_post_prompt.elapsed();
//         println!(
//             "\n\n{:4} prompt tokens processed: {:.2} token/s",
//             prompt_tokens.len(),
//             prompt_tokens.len() as f64 / prompt_dt.as_secs_f64(),
//         );
//         println!(
//             "{:4} tokens generated: {:.2} token/s",
//             to_sample,
//             to_sample as f64 / dt.as_secs_f64(),
//         );
//
//         match prompt {
//             Prompt::One(_) => break,
//             Prompt::Interactive => {}
//             Prompt::Chat => {
//                 pre_prompt_tokens = [prompt_tokens.as_slice(), all_tokens.as_slice()].concat()
//             }
//         }
//     }
//
//     Ok(())
// }
