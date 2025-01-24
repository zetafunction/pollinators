use indicatif::{ProgressBar, ProgressFinish, ProgressStyle};

pub fn new_spinner() -> ProgressBar {
    ProgressBar::new_spinner()
        .with_style(
            ProgressStyle::with_template("[{spinner:20.cyan/blue}] {msg}")
                .unwrap()
                .tick_strings(&[
                    ">                   ",
                    "=>                  ",
                    "==>                 ",
                    " ==>                ",
                    "  ==>               ",
                    "   ==>              ",
                    "    ==>             ",
                    "     ==>            ",
                    "      ==>           ",
                    "       ==>          ",
                    "        ==>         ",
                    "         ==>        ",
                    "          ==>       ",
                    "           ==>      ",
                    "            ==>     ",
                    "             ==>    ",
                    "              ==>   ",
                    "               ==>  ",
                    "                ==> ",
                    "                 ==>",
                    "                  ==",
                    "                   =",
                    "                    ",
                    "====================",
                ]),
        )
        .with_finish(ProgressFinish::AndLeave)
}

pub fn new_bar(len: u64) -> ProgressBar {
    ProgressBar::new(len)
        .with_style(
            ProgressStyle::with_template("[{bar:20.cyan/blue}] {msg} {pos}/{len}")
                .unwrap()
                .progress_chars("=> "),
        )
        .with_finish(ProgressFinish::AndLeave)
}
