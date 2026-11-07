use super::Status;
use anyhow::{Context, Result, bail};
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSError, NSString};

const PLIST_NAME: &str = "com.takeapanel.launcher.autostart.plist";

// SMAppService is looked up at runtime so the app still runs on macOS 11/12.
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

fn service_class() -> Result<&'static AnyClass> {
    AnyClass::get(c"SMAppService").context("系统开机自启需要 macOS 13 或更新版本")
}

fn service() -> Result<Retained<AnyObject>> {
    let class = service_class()?;
    let executable = std::env::current_exe().context("无法确定应用路径")?;
    let contents = executable.parent().and_then(|path| path.parent())
        .filter(|path| path.file_name().is_some_and(|name| name == "Contents"))
        .context("请使用 scripts/package-macos.sh 打包，并从签名后的 Take a Panel.app 中运行以管理开机自启")?;
    if !contents
        .join("Library/LaunchAgents")
        .join(PLIST_NAME)
        .is_file()
    {
        bail!("应用缺少开机自启配置，请使用 scripts/package-macos.sh 重新打包");
    }
    let name = NSString::from_str(PLIST_NAME);
    // SAFETY: This macOS 13+ class has been resolved at runtime. The method
    // accepts an NSString and returns an autoreleased SMAppService instance.
    let service = unsafe { msg_send![class, agentServiceWithPlistName: &*name] };
    Ok(service)
}

fn read_status(service: &AnyObject) -> Result<Status> {
    // SAFETY: service is an SMAppService; status has the NSInteger ABI.
    let status: isize = unsafe { msg_send![service, status] };
    match status {
        // An agent that has never been registered can also report NotFound.
        // service() already checked that its bundled plist exists; keep the
        // request button available and let register report signing/OS errors.
        0 | 3 => Ok(Status::Disabled),
        1 => Ok(Status::Enabled),
        2 => Ok(Status::RequiresApproval),
        value => bail!("系统返回未知的开机自启状态：{value}"),
    }
}

pub(super) fn status() -> Result<Status> {
    objc2::rc::autoreleasepool(|_| read_status(&*service()?))
}

pub(super) fn set_enabled(enabled: bool) -> Result<()> {
    objc2::rc::autoreleasepool(|_| {
        let service = service()?;
        let current = read_status(&service)?;
        if (enabled && current != Status::Disabled) || (!enabled && current == Status::Disabled) {
            return Ok(());
        }
        // SAFETY: Both selectors accept an autoreleasing NSError** and return
        // BOOL. objc2 handles NSError ownership through its trailing `_` form.
        let result: Result<(), Retained<NSError>> = unsafe {
            if enabled {
                msg_send![&*service, registerAndReturnError: _]
            } else {
                msg_send![&*service, unregisterAndReturnError: _]
            }
        };
        if let Err(error) = result {
            // A denied registration may still leave a valid pending request.
            if enabled && read_status(&service)? == Status::RequiresApproval {
                return Ok(());
            }
            let description = error.localizedDescription();
            bail!("系统拒绝修改开机自启：{description}");
        }
        Ok(())
    })
}

pub(super) fn open_settings() -> Result<()> {
    let class = service_class()?;
    // SAFETY: This class method is available with SMAppService on macOS 13+.
    unsafe {
        let _: () = msg_send![class, openSystemSettingsLoginItems];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "run this test executable inside a signed app bundle on macOS 13+"]
    fn signed_bundle_can_query_native_service_without_registering() {
        // This probe only reads state; it never registers or unregisters a job.
        super::status().unwrap();
    }

    #[test]
    fn bundled_agent_starts_a_hidden_client_instead_of_the_managed_server() {
        let value = plist::Value::from_reader_xml(
            include_bytes!("../../packaging/macos/com.takeapanel.launcher.autostart.plist")
                .as_slice(),
        )
        .unwrap();
        let dict = value.as_dictionary().unwrap();
        assert_eq!(
            dict["BundleProgram"].as_string(),
            Some("Contents/MacOS/take-a-panel")
        );
        let args = dict["ProgramArguments"].as_array().unwrap();
        assert_eq!(args[1].as_string(), Some("--hidden"));
        assert_eq!(dict["RunAtLoad"].as_boolean(), Some(true));
        assert!(!dict.contains_key("KeepAlive"));
    }
}
