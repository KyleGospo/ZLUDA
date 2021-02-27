use crate::{llvm, translate, ModuleParser};
use hip_common::raytracing::VariablesBlock;
use hip_runtime_sys::*;
use hiprt_sys::*;
use lazy_static::lazy_static;
use paste::paste;
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::{env, ptr};
use zluda_llvm::bit_writer::*;

macro_rules! test_raytracing {
    ($file:ident, $fn_name:ident) => {
        paste! {
            #[test]
            #[allow(non_snake_case)]
            fn [<$file _  $fn_name>] () {
                let ptx = include_str!(concat!("ptx_raytracing/", stringify!($file), ".ptx"));
                let llvm_file_name = concat!("ptx_raytracing/", stringify!([<$file _  $fn_name>]), ".ll");
                let bitcode = include_bytes!(concat!("ptx_raytracing/", stringify!([<$file _  $fn_name>]), ".ll"));
                let fn_name = stringify!($fn_name);
                unsafe { test(ptx, bitcode, llvm_file_name, fn_name) }
            }
        }
    };
}

// HIP-RT is not thread safe
lazy_static! {
    static ref HIPRT_MUTEX: Mutex<HipRt> = Mutex::new(unsafe { HipRt::load() }.unwrap());
}

unsafe fn test(ptx_txt: &str, llvm_ir: &[u8], llvm_file_name: &str, fn_name: &str) {
    let mut errors = Vec::new();
    let ast = ModuleParser::new().parse(&mut errors, ptx_txt).unwrap();
    assert!(errors.len() == 0);
    let mut empty_attribute_variables = VariablesBlock::empty();
    let raytracing_module =
        translate::to_llvm_module_for_raytracing(ast, fn_name, &mut empty_attribute_variables)
            .unwrap();
    let llvm_bitcode_from_ptx = raytracing_module.compilation_module.get_bitcode_main();
    let mut llvm_ir_copy = llvm_ir.to_vec();
    llvm_ir_copy.push(0);
    let reference_llvm_ir_buffer = llvm::MemoryBuffer::create_no_copy(&*llvm_ir_copy, true);
    let reference_module = llvm::parse_ir_in_context(
        &raytracing_module.compilation_module._llvm_context,
        reference_llvm_ir_buffer,
    )
    .unwrap();
    let reference_llvm_bitcode_buffer =
        llvm::MemoryBuffer::from_ffi(LLVMWriteBitcodeToMemoryBuffer(reference_module.get()));
    if reference_llvm_bitcode_buffer.as_slice() != llvm_bitcode_from_ptx.as_slice() {
        let ptx_string = raytracing_module.compilation_module.get_llvm_text();
        if ptx_string.as_cstr().to_bytes() != llvm_ir {
            if let Ok(dump_path) = env::var("ZLUDA_TEST_LLVM_DUMP_DIR") {
                let mut path = PathBuf::from(dump_path);
                path.push(llvm_file_name);
                if let Ok(()) = fs::create_dir_all(path.parent().unwrap()) {
                    fs::write(path, &*ptx_string.as_cstr().to_string_lossy()).ok();
                }
            }
            panic!("{}", ptx_string);
        }
    }
    assert_eq!(hipInit(0), hipError_t(0));
    let mut hip_context = ptr::null_mut();
    assert_eq!(hipCtxCreate(&mut hip_context, 0, 0), hipError_t(0));
    let mut context_input = hiprtContextCreationInput {
        ctxt: hip_context as _,
        device: 0,
        deviceType: hiprtDeviceType::hiprtDeviceAMD,
    };
    let mut context = ptr::null_mut();
    let hiprt = HIPRT_MUTEX.lock().unwrap();
    assert!(
        hiprt.hiprtCreateContext(
            hiprt_sys::HIPRT_API_VERSION,
            &mut context_input,
            &mut context
        ) == hiprtError(0)
    );
    let debug_level = if cfg!(debug_assertions) {
        b"-g\0".as_ptr()
    } else {
        b"-g0\0".as_ptr()
    };
    let options = [
        debug_level,
        // We just want to emit LLVM, we'd use O0, but somehow IR emitted by O0 prevents inling.
        // Weirdly, -disable-llvm-optzns produces much bigger code
        b"-O1\0".as_ptr(),
        // Stop compilation at LLVM
        b"-fgpu-rdc\0".as_ptr(),
        // hiprtc injects -mcumode which we don't want
        b"-mno-cumode\0".as_ptr(),
        // Internalization makes so that _rt_trace_time_mask_flags_64 is module-private
        // and does not get linked with the code generated by ptx compiler
        b"-mllvm\0".as_ptr(),
        b"-amdgpu-internalize-symbols=0\0".as_ptr(),
    ];
    let mut rt_program = ptr::null_mut::<c_void>();
    let headers = raytracing_module
        .headers
        .iter()
        .map(|s| s.as_ptr())
        .collect::<Vec<_>>();
    let header_names = raytracing_module
        .header_names
        .iter()
        .map(|s| s.as_ptr())
        .collect::<Vec<_>>();
    assert!(
        hiprt.hiprtBuildTraceProgram(
            context,
            translate::RaytracingModule::KERNEL_NAME.as_ptr(),
            raytracing_module.kernel_source.as_ptr() as _,
            "zluda_rt_kernel\0".as_ptr() as _,
            headers.len() as i32,
            headers.as_ptr() as _,
            header_names.as_ptr() as _,
            options.as_ptr() as _,
            options.len() as i32,
            (&mut rt_program) as *mut _ as _,
        ) == hiprtError(0)
    );
    // It would be intresting to compile into a relocatable to check for
    // fn declaration implementation mismatch between C++ wrapper and emitted
    // bitcode, but unfortunately in case of mismatches LLVM linker just
    // bitcasts incompatible function (which makes sense to a certain degree)
}

test_raytracing!(optixHello_generated_draw_color, draw_solid_color);
test_raytracing!(
    optixHello_generated_draw_color_var_ptr_cast,
    draw_solid_color
);
test_raytracing!(optixSphere_generated_sphere, bounds);
test_raytracing!(optixSphere_generated_sphere, robust_intersect);
test_raytracing!(optixSphere_generated_normal_shader, closest_hit_radiance);
test_raytracing!(optixPathTracer_generated_disney, Eval);
test_raytracing!(optixCallablePrograms_generated_optixCallablePrograms, miss);
test_raytracing!(optixPathTracer_generated_hit_program, closest_hit);