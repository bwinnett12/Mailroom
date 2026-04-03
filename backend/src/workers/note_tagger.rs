use serde::{Deserialize, Serialize};
use loco_rs::prelude::*;
use sea_orm::entity::prelude::*;
use crate::models::_entities::notes;

pub struct Worker {
    pub ctx: AppContext,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NoteTaggerArgs {
    pub note_id: i32,
}

#[async_trait]
impl BackgroundWorker<NoteTaggerArgs> for Worker {
    fn build(ctx: &AppContext) -> Self {
        Self { ctx: ctx.clone() }
    }

    fn class_name() -> String {
        "NoteTagger".to_string()
    }

    fn tags() -> Vec<String> {
        Vec::new()
    }

    async fn perform(&self, vars: NoteTaggerArgs) -> Result<()> {
        // 1. Fetch the note - explicitly type the result
        let note: Option<notes::Model> = notes::Entity::find_by_id(vars.note_id)
            .one(&self.ctx.db)
            .await
            .map_err(|e| Error::msg(e))?;

        // 2. Setup the AI Request
        let client = reqwest::Client::new();

        let ai_response = client
            .post("http://10.0.1.10:8090/v1/chat/completions")
            .json(&serde_json::json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": format!("Summarize this into 3 comma-separated tags: {}", note.content.clone().unwrap_or_default())}]
            }))
            .send()
            .await
            .map_err(|e: reqwest::Error| Error::string(&e.to_string()))?;

        // 3. Extract JSON
        let res_body = ai_response
            .json::<serde_json::Value>()
            .await
            .map_err(|e: reqwest::Error| Error::string(&e.to_string()))?;


        println!("DEBUG: LocalAI Response: {:?}", res_body);

        // 4. Extract text from AI response
        let tags = res_body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("ai-processed");


        println!("DEBUG: Extracted Tags: {}", tags);

        // 5. Update the record
        let mut note: notes::ActiveModel = note.into();
        note.tags = Set(Some(tags.to_string()));
        note.update(&self.ctx.db).await.map_err(|e| Error::msg(e))?;

        Ok(())
    }
}
