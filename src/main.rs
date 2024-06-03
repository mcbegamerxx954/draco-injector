use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Cursor, Read, Write},
    mem,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use apk::{
    res::{Chunk, ResValue, ResValueType, ResXmlAttribute},
    Apk,
};
use clap::{
    builder::{
        styling::{AnsiColor, Style},
        Styles,
    },
    CommandFactory, FromArgMatches, Parser,
};
use console::{style, Emoji};
use indicatif::{ProgressBar, ProgressIterator, ProgressStyle};
use inquire::{validator::Validation, Confirm};
use object_rewrite::Rewriter;
use zip::{read::ZipFile, write::ExtendedFileOptions, ZipArchive, ZipWriter};
#[derive(Parser)]
#[clap(name = "Mc injector", version = "0.0.1")]
#[command(version, about, long_about = None, styles = get_style())]
struct Options {
    /// Apk file to patch
    #[clap(required = true)]
    apk: PathBuf,
    /// New app name
    #[arg(short, long)]
    appname: Option<String>,
    /// New package id
    #[arg(short, long)]
    pkgid: Option<String>,
    /// Remove songs from final apk
    #[arg(short, long)]
    remove_songs: bool,
    /// Output path
    #[arg(short, long, required = true)]
    output: PathBuf,
}
const MUSIC_PATH: &str = "assets/assets/resource_packs/vanilla_music";
const fn get_style() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightYellow.on_default())
        .usage(AnsiColor::Green.on_default())
        .literal(Style::new().fg_color(None).bold())
        .placeholder(AnsiColor::Green.on_default())
}
fn main() -> Result<()> {
    let options = Options::parse();
    let file = File::open(&options.apk)?;
    rewrite_zip(&file, &options.output, &options)?;
    println!("{} Signing patched apk", Emoji("🖋️", ""));
    Apk::sign(&options.output, None)?;
    println!("{}", style("Done!").green().bold());
    Ok(())
}

fn rewrite_zip(zip_file: &File, output: &Path, opts: &Options) -> Result<()> {
    let mut zip = ZipArchive::new(BufReader::new(zip_file))?;
    let output = File::create_new(output).with_context(|| "Output file already exists")?;
    let mut outzip = ZipWriter::new(BufWriter::new(output));
    println!(
        "{}: Make sure you use shaders for the version of apk you are patching",
        style("TIP").green()
    );
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("{} Patching apk file", Emoji("📦", ""));
    let pstyle = ProgressStyle::with_template(
        "{percent:.green.bold}% {msg} [{bar:30.cyan/yellow}] {elapsed}",
    )?
    .progress_chars("#- ");
    let pbar = ProgressBar::new(zip.len().try_into().unwrap())
        .with_style(pstyle)
        .with_message("Patching apk")
        .with_finish(indicatif::ProgressFinish::Abandon);
    pbar.enable_steady_tick(std::time::Duration::from_millis(250));
    for i in (0..zip.len()).progress_with(pbar.clone()) {
        let mut file = zip.by_index(i)?;
        if skip_filename(file.name(), opts.remove_songs) {
            continue;
        }
        if file.name() == "AndroidManifest.xml" {
            if opts.appname.is_none() && opts.pkgid.is_none() {
                pbar.suspend(|| {
                    println!("{} Leaving app and package name the same", Emoji("⏩", ""))
                });
                outzip.raw_copy_file(file)?;
                continue;
            }
            pbar.suspend(|| println!("{} Editing app and package name", Emoji("📝", "")));
            let mut axml = Vec::new();
            file.read_to_end(&mut axml)?;
            let mod_axml = edit_manifest(&axml, opts.appname.as_deref(), opts.pkgid.as_deref())?;
            outzip.start_file(
                file.name(),
                zip::write::FileOptions::<ExtendedFileOptions>::default(),
            )?;
            outzip.write_all(&mod_axml)?;
            continue;
        }
        // Boo hoo alignment & compression
        if file.name() == "resources.arsc" {
            let options = zip::write::FileOptions::<ExtendedFileOptions>::default()
                .compression_method(zip::CompressionMethod::Stored)
                .with_alignment(4);
            outzip.start_file(file.name(), options)?;
            std::io::copy(&mut file, &mut outzip)?;
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
fn skip_filename(filename: &str, no_songs: bool) -> bool {
    (no_songs && filename.starts_with(MUSIC_PATH)) ||
    // Broken ignature v1 can cause issues
    filename.starts_with("META-INF/") && 
    (filename.ends_with(".SF") || filename.ends_with("RSA")) || 
    // Skip this lib so the patcher can update already patched apps
    filename.ends_with("libdraco_redirector.so")
}
fn patch_minecraft(
    lib: &mut ZipFile,
    zip: &mut ZipWriter<BufWriter<File>>,
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

fn edit_manifest(manifest: &[u8], name: Option<&str>, pkg_name: Option<&str>) -> Result<Vec<u8>> {
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
            if let Some(value) = get_attribute_value(provider_attrs, "authorities", strings) {
                let string = &mut strings[value.data as usize];
                if let Some(suffix) = string.strip_prefix(&old_pkgname) {
                    *string = pkg_name.unwrap().to_owned() + suffix;
                }
            }
        }
    }
    // Editing resources.arsc is hard
    if let Some(app_name) = name {
        edit_attr_in_element(chunks, "application", "label", app_name.to_owned(), strings)?;
        edit_attr_in_element(chunks, "activity", "label", app_name.to_owned(), strings)?;
    }
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
            "lib/arm64-v8a/libminecraftpe.so" => Some(Self::Aarch64),
            "lib/x86/libminecraftpe.so" => Some(Self::X86),
            "lib/x86_64/libminecraftpe.so" => Some(Self::X86_64),
            _ => None,
        }
    }
    const fn rust_target(&self) -> &str {
        match self {
            Self::Aarch64 => "aarch64-linux-android",
            Self::Armv7a => "armv7-linux-androideabi",
            Self::X86 => "i686-linux-android",
            Self::X86_64 => "x86_64-linux-android",
        }
    }
    const fn android_abi(&self) -> &str {
        match self {
            Self::Aarch64 => "arm64-v8a",
            Self::Armv7a => "armeabi-v7a",
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
        }
    }
}
