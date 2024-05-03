use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    mem,
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

    // Change package name
    if let Some(pkgname) = pkg_name {
        let old_pkgname =
            edit_attr_in_element(chunks, "manifest", "package", pkgname.to_owned(), strings)?
                .with_context(|| "There is no package name in manifest.")?;

        // Get rid of conflicts
        let providers: Vec<&mut Vec<ResXmlAttribute>> = chunks
            .iter_mut()
            .filter_map(|c| parse_element(c, "provider", strings))
            .collect();

        for provider_attrs in providers {
            if let Some(value) = get_attribute_value(&provider_attrs, "authorities", strings) {
                let string = &mut strings[value.data as usize];
                if let Some(suffix) = string.strip_prefix(&old_pkgname) {
                    *string = pkg_name.unwrap().to_owned() + suffix;
                }
            }
        }
    }
    // Editing resources.arsc is hard
    edit_attr_in_element(chunks, "application", "label", name.to_owned(), strings)?;
    edit_attr_in_element(chunks, "activity", "label", name.to_owned(), strings)?;
    // Return modified manifest
    let mut mod_manifest = Vec::new();
    Chunk::Xml(xchunks).write(&mut Cursor::new(&mut mod_manifest))?;
    Ok(mod_manifest)
}
fn edit_attr_in_element(
    elements: &mut [Chunk],
    el_name: &str,
    attr_name: &str,
    new_str: String,
    pool: &mut Vec<String>,
) -> Result<Option<String>> {
    let attrs = elements
        .iter_mut()
        .find_map(|e| parse_element(&mut *e, el_name, pool))
        .with_context(|| format!("Xml element is missing: {el_name}"))?;
    let attr = attrs
        .iter_mut()
        .find(|a| attr_has_name(a.name, attr_name, pool))
        .with_context(|| format!("Attribute {attr_name} not found in element {el_name}"))?;

    edit_attr_string(attr, new_str, pool)
}
fn edit_attr_string(
    attr: &mut ResXmlAttribute,
    name: String,
    pool: &mut Vec<String>,
) -> Result<Option<String>> {
    let value = &mut attr.typed_value;
    let attr_type = ResValueType::from_u8(value.data_type)
        .with_context(|| format!("Type of label value is unknown: {}", value.data_type))?;
    match attr_type {
        ResValueType::String => Ok(Some(mem::replace(&mut pool[value.data as usize], name))),
        // In this case we overwrite it so that its a direct string, rid solving is pain
        _ => {
            let new_rvalue = ResValue {
                size: 8,
                res0: 0,
                data_type: ResValueType::String as u8,
                data: pool.len() as u32,
            };
            *value = new_rvalue;
            attr.raw_value = pool.len() as i32;
            pool.push(name);
            Ok(None)
        }
    }
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
