mod ai;
mod app;
mod board_view;
mod config_ui;
mod game;
mod llm_ai;

#[cfg(test)]
mod test_support;

pub fn window_conf() -> macroquad::miniquad::conf::Conf {
    board_view::window_conf()
}

pub async fn run() {
    app::App::new().run().await;
}
