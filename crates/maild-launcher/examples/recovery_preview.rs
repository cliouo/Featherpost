//! Isolated visual fixture: no daemon, platform configuration, filesystem repair or login items.
use maild_launcher::{Controller, Operation, Status};
struct Preview {
    online: bool,
    repaired: bool,
}
impl Controller for Preview {
    fn status(&mut self) -> Status {
        Status {
            online: self.online,
            mailbox_available: self.online && self.repaired,
            can_manage: true,
            login: "未注册（预览）".into(),
        }
    }
    fn perform(&mut self, operation: Operation) -> Result<&'static str, &'static str> {
        match operation {
            Operation::StartOnce => {
                self.online = true;
                Ok("预览：后台已连接")
            }
            Operation::Stop => {
                self.online = false;
                Ok("预览：后台已停止")
            }
            Operation::Repair => {
                self.repaired = true;
                Ok("预览：内置前端已恢复，原包已保留备份")
            }
            Operation::OpenMailbox if self.online && self.repaired => Ok("预览：已请求打开邮箱"),
            Operation::OpenMailbox => Err("邮件前端尚不可用，请先恢复内置前端或启动后台"),
            _ => Ok("预览：没有更改系统登录项"),
        }
    }
}
fn main() {
    maild_launcher::show_recovery(&mut Preview {
        online: false,
        repaired: false,
    });
}
