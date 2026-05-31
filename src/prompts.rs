//! # MCP Prompts
//!
//! Predefined templates for grounded question-answering workflows.
//!
//! ## Rationale
//! Encourages agents to follow a standard "search-then-cite" workflow. These prompts
//! guide the model to use specific tools (`spark.search`, `spark.get_chunk`) and
//! cite their evidence consistently, improving the reliability of RAG answers.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{GetPromptResult, PromptMessage, PromptMessageRole};
use rmcp::prompt;
use rmcp::prompt_router;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::server::SparkMcp;

/// Arguments for `spark.grounded_answer`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GroundedPromptArgs {
    /// The user's question to be answered using the corpus.
    #[serde(default)]
    pub question: Option<String>,
}

#[prompt_router(router = "prompt_router_spark", vis = "pub")]
impl SparkMcp {
    /// Grounded SPARK/Ada answer template using spark.search + spark.get_chunk.
    #[prompt(
        name = "spark.grounded_answer",
        description = "Grounded SPARK/Ada answer template (use spark.search + spark.get_chunk)."
    )]
    async fn spark_grounded_answer(
        &self,
        Parameters(args): Parameters<GroundedPromptArgs>,
    ) -> GetPromptResult {
        let mut text = String::from(
            "You are a SPARK/Ada assistant. Before answering:\n\
- Call spark.search with a precise query.\n\
- For each citation, call spark.get_chunk with doc_id + chunk_index.\n\
- Use only chunk text for claims; cite doc_id#chunk_index.\n\
- Keep tool output quiet by default (include_context=false unless needed).\n\
- Example: spark.search { \"query\": \"...\", \"mode\": \"lexical\", \"include_context\": false }\n",
        );

        if let Some(question) = args
            .question
            .as_ref()
            .map(|q| q.trim())
            .filter(|q| !q.is_empty())
        {
            text.push_str("\nUser question:\n");
            text.push_str(question);
        }

        GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)])
            .with_description("Grounded SPARK/Ada answer template")
    }

    /// Grounded SPARK/Ada answer template with checklist + structured response.
    #[prompt(
        name = "spark.grounded_answer_checklist",
        description = "Grounded SPARK/Ada answer template with checklist + response format."
    )]
    async fn spark_grounded_answer_checklist(
        &self,
        Parameters(args): Parameters<GroundedPromptArgs>,
    ) -> GetPromptResult {
        let mut text = String::from(
            "You are a SPARK/Ada assistant. Use this checklist before answering:\n\
1) Call spark.search with a focused query.\n\
2) Select 1-3 high-signal results.\n\
3) Call spark.get_chunk for each result.\n\
4) Answer using only chunk text.\n\
5) Cite each claim as doc_id#chunk_index.\n\
6) Keep tool output quiet by default (include_context=false unless needed).\n\
   Example: spark.search { \"query\": \"...\", \"mode\": \"lexical\", \"include_context\": false }\n\
\nResponse format:\n\
- Answer: <concise response>\n\
- Evidence:\n\
  - <doc_id#chunk_index>: <short supporting quote>\n\
  - <doc_id#chunk_index>: <short supporting quote>\n\
- Gaps/Assumptions: <if any>\n",
        );

        if let Some(question) = args
            .question
            .as_ref()
            .map(|q| q.trim())
            .filter(|q| !q.is_empty())
        {
            text.push_str("\nUser question:\n");
            text.push_str(question);
        }

        GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)])
            .with_description("Grounded SPARK/Ada answer template with checklist + response format")
    }
}
