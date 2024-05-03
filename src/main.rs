use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::Path,
};

use anyhow::{Context, Result};
use apk::{
    res::{Chunk, ResValue, ResValueType, ResXmlAttribute},
    Apk,
};
use clap::{CommandFactory, FromArgMatches, Parser};
use console::{style, Emoji};
use human_bytes::human_bytes;
use indicatif::{ProgressBar, ProgressIterator, ProgressStyle};
use inquire::{validator::Validation, Confirm};
use object_rewrite::Rewriter;
use zip::{read::ZipFile, write::ExtendedFileOptions, ZipArchive, ZipWriter};
#[derive(Parser)]
#[clap(name = "Injector", version = "0.0.1")]
#[command(version, about, long_about = None)]
struct Options {
    /// Folders/files to work with
    apk: String,
    /// The new name of the apk
    #[arg(short, long, default_value = "Minecraft Patched(whar)")]
    appname: String,
    /// New package name(optional)
    #[arg(short, long)]
    pkgname: Option<String>,
    /// Where to place the output at
    #[arg(short, long)]
    output: String,
}
const MUSIC_PATH: &str = "assets/assets/resource_packs/vanilla_music";
fn main() -> Result<()> {
    let options = Options::command()
        .arg_required_else_help(true)
        .get_matches();
    let options = Options::from_arg_matches(&options)?;
    let file = File::open(&options.apk)?;
    rewrite_zip(&file, Path::new(&options.output), &options)?;
    println!("{} Signing patched apk", Emoji("🖋️", ""));
    Apk::sign(Path::new(&options.output), None)?;
    println!("{}", style("Done!").green().bold());
    Ok(())
}

fn rewrite_zip(zip_file: &File, output: &Path, opts: &Options) -> Result<()> {
    let mut zip = ZipArchive::new(zip_file)?;
    let output = File::create_new(output)?;
    let mut outzip = ZipWriter::new(output);
    println!("{} Patching apk file", Emoji("📦", ""));
    let pstyle = ProgressStyle::with_template(
        "{percent:.green.bold}% {msg} [{bar:30.cyan/yellow}] {elapsed}",
    )?
    .progress_chars("#- ");
    let pbar = ProgressBar::new(zip.len().try_into().unwrap())
        .with_style(pstyle)
        .with_message("Patching apk")
        .with_finish(indicatif::ProgressFinish::Abandon);
    let mut total_size = 0;
    let mut remove_music = false;
    for i in 0..zip.len() {
        let file = zip.by_index(i)?;
        if file.name().starts_with(MUSIC_PATH) {
            total_size += file.compressed_size();
        }
    }
    if total_size > 0 {
        remove_music = Confirm::new(&format!(
            "remove vanilla songs to save {} from the final apk?",
            human_bytes(total_size as f64)
        ))
        .prompt()?;
    }
    for i in (0..zip.len()).progress_with(pbar.clone()) {
        let mut file = zip.by_index(i)?;
        if remove_music && file.name().starts_with(MUSIC_PATH) {
            continue;
        }
        if file.name() == "AndroidManifest.xml" {
            pbar.suspend(|| println!("{} Editing app and package name", Emoji("📝", "")));
            let mut axml = Vec::new();
            file.read_to_end(&mut axml)?;
            let axml = edit_manifest(&axml, &opts.appname, opts.pkgname.as_deref())?;
            outzip.start_file(
                file.name(),
                zip::write::FileOptions::<ExtendedFileOptions>::default(),
            )?;
            outzip.write_all(&axml)?;
            continue;
        }
        let libarch = match LibraryArch::from_str(file.name()) {
            Some(lib) => lib,
            None => {
                outzip.raw_copy_file(file)?;
                continue;
            }
        };
        pbar.suspend(|| {
            println!(
                "{} Patching minecraft {}",
                Emoji("🩹", ""),
                style(libarch.android_abi()).yellow().bold()
            )
        });
        patch_minecraft(&mut file, &mut outzip, libarch)?;
    }
    outzip.finish()?;
    Ok(())
}
fn patch_minecraft(
    lib: &mut ZipFile,
    zip: &mut ZipWriter<File>,
    libarch: LibraryArch,
) -> Result<()> {
    let libpatcher = get_draco_patch(libarch)?;
    let mut libmcpe = Vec::new();
    lib.read_to_end(&mut libmcpe)?;
    let libredirector = "libdraco_redirector.so";
    let mut elf = Rewriter::read(libmcpe.as_slice())?;
    elf.elf_add_needed(&[libredirector.as_bytes().to_vec()])?;
    let zipoptions = zip::write::FileOptions::<ExtendedFileOptions>::default();
    //Add patched minecraft lib
    zip.start_file(lib.name(), zipoptions.clone())?;
    elf.write(&mut *zip)?;
    // Add patcher library
    let libpath = "lib/".to_string() + libarch.android_abi() + "/" + libredirector;
    zip.start_file(libpath, zipoptions)?;
    zip.write_all(&libpatcher)?;
    Ok(())
}

fn edit_manifest(manifest: &[u8], name: &str, pkg_name: Option<&str>) -> Result<Vec<u8>> {
    let mut reader = Cursor::new(manifest);
    let Chunk::Xml(mut xchunks) = Chunk::parse(&mut reader)? else {
        anyhow::bail!("invalid manifest 0");
    };
    let (string_pool, chunks) = xchunks.split_first_mut().unwrap();
    let Chunk::StringPool(strings, _) = string_pool else {
        anyhow::bail!("Annoying....");
    };
    let mut old_pkg = None;
    // Change package name
    let manifest_attrs = chunks
        .iter_mut()
        .find_map(|c| parse_element(c, "manifest", strings))
        .with_context(|| "Cant find manifest root element")?;
    if pkg_name.is_some() {
        if let Some(value) = get_attribute_value(manifest_attrs, "package", strings) {
            assert!(value.data_type == ResValueType::String as u8);
            old_pkg = Some(strings[value.data as usize].clone());
            strings[value.data as usize] = pkg_name.unwrap().to_owned();
        };
    }
    // The reason this is like this is that
    // Mojang devs decided to put app name in resources.arsc
    // Trying to edit it made me wanna cry
    let application_attrs = chunks
        .iter_mut()
        .find_map(|c| parse_element(c, "application", strings))
        .with_context(|| "Cant find application element")?;

    if let Some(attr) = application_attrs
        .iter_mut()
        .find(|a| attr_has_name(a.name, "label", strings))
    {
        let value = &mut attr.typed_value;
        let attr_type = ResValueType::from_u8(value.data_type)
            .with_context(|| format!("Type of label value is unknown: {}", value.data_type))?;
        match attr_type {
            ResValueType::String => strings[value.data as usize] = name.to_owned(),
            // In this case we overwrite it so that its a direct string, rid solving is pain
            _ => {
                let new_rvalue = ResValue {
                    size: 8,
                    res0: 0,
                    data_type: ResValueType::String as u8,
                    data: strings.len() as u32,
                };
                *value = new_rvalue;
                attr.raw_value = strings.len() as i32;
                strings.push(name.to_owned());
            }
        }
    }
    // Get rid of conflicts
    let providers: Vec<&mut Vec<ResXmlAttribute>> = chunks
        .iter_mut()
        .filter_map(|c| parse_element(c, "provider", strings))
        .collect();
    let old_pkg = old_pkg.expect("Apk has no package name???");
    for provider_attrs in providers {
        if let Some(value) = get_attribute_value(&provider_attrs, "authorities", strings) {
            let string = &mut strings[value.data as usize];
            if let Some(suffix) = string.strip_prefix(&old_pkg) {
                *string = pkg_name.unwrap().to_owned() + suffix;
            }
        }
    }
    // Return modified manifest
    let mut mod_manifest = Vec::new();
    Chunk::Xml(xchunks).write(&mut Cursor::new(&mut mod_manifest))?;
    Ok(mod_manifest)
}
fn get_attribute_value(attrs: &[ResXmlAttribute], name: &str, pool: &[String]) -> Option<ResValue> {
    attrs
        .iter()
        .find(|a| attr_has_name(a.name, name, pool))
        .map(|a| a.typed_value)
}
fn attr_has_name(index: i32, name: &str, string_pool: &[String]) -> bool {
    let index = match usize::try_from(index) {
        Ok(usize) => usize,
        Err(_) => return false,
    };
    string_pool.get(index).is_some_and(|s| s == name)
}
fn parse_element<'c>(
    chunk: &'c mut Chunk,
    name: &str,
    string_pool: &[String],
) -> Option<&'c mut Vec<ResXmlAttribute>> {
    let Chunk::XmlStartElement(_, el, attrs) = chunk else {
        return None;
    };
    if string_pool.get(el.name as usize).is_some_and(|s| s == name) {
        return Some(attrs);
    }
    None
}

fn get_draco_patch(libarch: LibraryArch) -> Result<Vec<u8>> {
    let rust_target = libarch.rust_target();
    let libname = "libmcbe_r_".to_owned() + rust_target + ".so";
    let url = "https://github.com/mcbegamerxx954/mcbe_shader_redirector/releases/latest/download/"
        .to_owned()
        + &libname;
    let buff = match ureq::get(&url).call() {
        Ok(resp) => {
            assert!(resp.has("Content-length"));

            let len: usize = resp.header("Content-Length").unwrap().parse()?;
            let mut buf: Vec<u8> = Vec::with_capacity(len);
            resp.into_reader().read_to_end(&mut buf)?;
            buf
        }
        Err(e) => {
            let validator = |input: &str| {
                if !Path::new(input).exists() {
                    Ok(Validation::Invalid("Not a valid path".into()))
                } else {
                    Ok(Validation::Valid)
                }
            };
            let text = inquire::Text::new(&format!(
                "Cant download file, error: {e} \n\
                Specify path to get lib with target {rust_target}"
            ))
            .with_validator(validator)
            .prompt()?;
            fs::read(text)?
        }
    };

    Ok(buff)
}

//Funny utility
#[derive(Debug, Clone, Copy)]
enum LibraryArch {
    Aarch64,
    Armv7a,
    X86,
    X86_64,
}
impl LibraryArch {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "lib/armeabi-v7a/libminecraftpe.so" => Some(Self::Armv7a),
            "lib/aarch64/libminecraftpe.so" => Some(Self::Aarch64),
            "lib/x86/libminecraftpe.so" => Some(Self::X86),
            "lib/x86_64/libminecraftpe.so" => Some(Self::X86_64),
            _ => None,
        }
    }
    fn rust_target(&self) -> &str {
        match self {
            LibraryArch::Aarch64 => "aarch64-linux-android",
            LibraryArch::Armv7a => "armv7-linux-androideabi",
            LibraryArch::X86 => "i686-linux-android",
            LibraryArch::X86_64 => "x86_64-linux-android",
        }
    }
    fn android_abi(&self) -> &str {
        match self {
            LibraryArch::Aarch64 => "arm64-v8a",
            LibraryArch::Armv7a => "armeabi-v7a",
            LibraryArch::X86 => "x86",
            LibraryArch::X86_64 => "x86_64",
        }
    }
}
