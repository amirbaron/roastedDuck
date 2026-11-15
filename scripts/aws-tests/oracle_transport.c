// Test-only macOS transport isolation for the unmodified C++ AWS provider.
// Redirect only named fixture hosts and restrict IP connections to loopback.
// Add a process-local CA anchor; normal signature/hostname verification remains.
#include <Security/Security.h>
#include <Network/Network.h>
#include <CoreFoundation/CoreFoundation.h>
#include <sys/socket.h>
#include <netdb.h>
#include <arpa/inet.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>

static int allowed(const char *host) {
    return host && (!strcmp(host,"sts.eu-west-1.amazonaws.com") ||
      !strcmp(host,"sts.us-east-1.amazonaws.com") || !strcmp(host,"sts.amazonaws.com") ||
      !strcmp(host,"portal.sso.eu-west-1.amazonaws.com") ||
      !strcmp(host,"oidc.eu-west-1.amazonaws.com") || !strcmp(host,"oidc.us-east-2.amazonaws.com"));
}
static const char *s3_port(const char *host) {
    const char *port=getenv("ROASTEDDUCK_FIXTURE_S3_PORT");
    if(!port || atoi(port)<=0 || atoi(port)>65535 || !host)return NULL;
    const char *suffix=".duckdb-minio.com";size_t n=strlen(host),m=strlen(suffix);
    return !strcmp(host,"duckdb-minio.com") || (n>m && !strcmp(host+n-m,suffix)) ? port : NULL;
}
static const char *metadata_port(const char *host) {
    const char *port=getenv("ROASTEDDUCK_FIXTURE_HTTP_PORT");
    if(!port || atoi(port)<=0 || atoi(port)>65535)return NULL;
    return !strcmp(host,"169.254.170.2") || !strcmp(host,"169.254.170.23") || !strcmp(host,"169.254.169.254") ? port : NULL;
}
static void route_metadata(struct sockaddr_in *address) {
    const char *port=metadata_port(inet_ntoa(address->sin_addr));
    if(port && ntohs(address->sin_port)==80){address->sin_addr.s_addr=htonl(INADDR_LOOPBACK);address->sin_port=htons((unsigned short)atoi(port));}
}
static nw_endpoint_t fixture_endpoint(const char *host,const char *port) {
    if(s3_port(host) && !strcmp(port,"9000"))return nw_endpoint_create_host("127.0.0.1",s3_port(host));
    if(metadata_port(host) && !strcmp(port,"80"))return nw_endpoint_create_host("127.0.0.1",metadata_port(host));
    if(allowed(host)) {
        fprintf(stderr,"fixture endpoint: %s:%s\n",host,port);
        return nw_endpoint_create_host("127.0.0.1",getenv("ROASTEDDUCK_FIXTURE_TLS_PORT"));
    }
    if(!strcmp(host,"127.0.0.1") || !strcmp(host,"localhost") || !strcmp(host,"::1"))return nw_endpoint_create_host(host,port);
    fprintf(stderr,"fixture denied endpoint: %s\n",host);return nw_endpoint_create_host("127.0.0.1","1");
}
static FILE *fixture_fopen(const char *path,const char *mode) {
    if(strstr(path,"/sso/cache/"))fprintf(stderr,"fixture cache fopen: %s\n",path);
    if(!strcmp(path,"/etc/ssl/cert.pem")) {
        const char *ca=getenv("ROASTEDDUCK_FIXTURE_CA_PEM");if(ca)return fopen(ca,mode);
    }
    return fopen(path,mode);
}
static int fixture_getaddrinfo(const char *node,const char *service,const struct addrinfo *hints,struct addrinfo **out) {
    if (s3_port(node)) return getaddrinfo("127.0.0.1",service,hints,out);
    if (allowed(node)) {fprintf(stderr,"fixture DNS: %s\n",node);return getaddrinfo("127.0.0.1",service,hints,out);}
    if(node && metadata_port(node))return getaddrinfo(node,service,hints,out);
    if (!node || !strcmp(node,"localhost") || !strcmp(node,"127.0.0.1") || !strcmp(node,"::1"))
        return getaddrinfo(node,service,hints,out);
    fprintf(stderr,"fixture denied DNS: %s\n",node);return EAI_NONAME;
}
static int fixture_connect(int fd,const struct sockaddr *address,socklen_t length) {
    struct sockaddr_storage copy;
    if (length>sizeof(copy)) {errno=EINVAL;return -1;}
    memcpy(&copy,address,length);
    if(address->sa_family==AF_INET) {
        struct sockaddr_in *a=(struct sockaddr_in*)&copy;
        fprintf(stderr,"fixture connect: %s:%u\n",inet_ntoa(a->sin_addr),ntohs(a->sin_port));
        route_metadata(a);
        if(ntohl(a->sin_addr.s_addr)==INADDR_LOOPBACK && ntohs(a->sin_port)==9000 && s3_port("duckdb-minio.com"))a->sin_port=htons((unsigned short)atoi(s3_port("duckdb-minio.com")));
        if(ntohl(a->sin_addr.s_addr)!=INADDR_LOOPBACK) {errno=EACCES;return -1;}
        if(ntohs(a->sin_port)==443) a->sin_port=htons((unsigned short)atoi(getenv("ROASTEDDUCK_FIXTURE_TLS_PORT")));
    } else if(address->sa_family==AF_INET6) {
        struct sockaddr_in6 *a=(struct sockaddr_in6*)&copy;
        if(!IN6_IS_ADDR_LOOPBACK(&a->sin6_addr)) {errno=EACCES;return -1;}
    } else if(address->sa_family!=AF_UNIX) {errno=EACCES;return -1;}
    return connect(fd,(struct sockaddr*)&copy,length);
}
static int fixture_connectx(int fd,const sa_endpoints_t *endpoints,sae_associd_t association,unsigned int flags,const struct iovec *iov,unsigned int count,size_t *bytes,sae_connid_t *connection) {
    sa_endpoints_t copy=*endpoints;struct sockaddr_storage destination;
    if(endpoints->sae_dstaddrlen>sizeof(destination)){errno=EINVAL;return -1;}
    memcpy(&destination,endpoints->sae_dstaddr,endpoints->sae_dstaddrlen);
    if(destination.ss_family==AF_INET) {
        struct sockaddr_in *a=(struct sockaddr_in*)&destination;
        fprintf(stderr,"fixture connectx: %s:%u\n",inet_ntoa(a->sin_addr),ntohs(a->sin_port));
        route_metadata(a);
        if(ntohl(a->sin_addr.s_addr)==INADDR_LOOPBACK && ntohs(a->sin_port)==9000 && s3_port("duckdb-minio.com"))a->sin_port=htons((unsigned short)atoi(s3_port("duckdb-minio.com")));
        if(ntohl(a->sin_addr.s_addr)!=INADDR_LOOPBACK){errno=EACCES;return -1;}
        if(ntohs(a->sin_port)==443)a->sin_port=htons((unsigned short)atoi(getenv("ROASTEDDUCK_FIXTURE_TLS_PORT")));
    } else if(destination.ss_family==AF_INET6) {
        struct sockaddr_in6 *a=(struct sockaddr_in6*)&destination;
        if(!IN6_IS_ADDR_LOOPBACK(&a->sin6_addr)){errno=EACCES;return -1;}
    } else {errno=EACCES;return -1;}
    copy.sae_dstaddr=(struct sockaddr*)&destination;
    return connectx(fd,&copy,association,flags,iov,count,bytes,connection);
}
static CFArrayRef anchors;
static pthread_once_t once=PTHREAD_ONCE_INIT;
static void load_anchor(void) {
    const char *path=getenv("ROASTEDDUCK_FIXTURE_CA_DER");if(!path) return;
    FILE *f=fopen(path,"rb");if(!f) return;
    unsigned char data[16384];size_t n=fread(data,1,sizeof(data),f);fclose(f);
    CFDataRef der=CFDataCreate(NULL,data,(CFIndex)n);
    SecCertificateRef certificate=SecCertificateCreateWithData(NULL,der);CFRelease(der);
    if(certificate) {anchors=CFArrayCreate(NULL,(const void**)&certificate,1,&kCFTypeArrayCallBacks);CFRelease(certificate);}
}
static void anchor(SecTrustRef trust) {
    pthread_once(&once,load_anchor);if(!anchors)return;
    CFArrayRef policies=NULL;if(SecTrustCopyPolicies(trust,&policies)!=errSecSuccess)return;
    int accept=0;
    for(CFIndex i=0;i<CFArrayGetCount(policies);i++) {
        CFDictionaryRef properties=SecPolicyCopyProperties((SecPolicyRef)CFArrayGetValueAtIndex(policies,i));
        CFStringRef name=CFDictionaryGetValue(properties,kSecPolicyName);char text[256];
        if(name && CFGetTypeID(name)==CFStringGetTypeID() && CFStringGetCString(name,text,sizeof(text),kCFStringEncodingUTF8) && allowed(text))accept=1;
        CFRelease(properties);
    }
    CFRelease(policies);
    if(accept) {fprintf(stderr,"fixture CA anchor applied\n");SecTrustSetAnchorCertificates(trust,anchors);SecTrustSetAnchorCertificatesOnly(trust,true);}
}
static Boolean fixture_evaluate(SecTrustRef trust,CFErrorRef *error) {anchor(trust);Boolean ok=SecTrustEvaluateWithError(trust,error);if(!ok && error && *error) {CFStringRef description=CFErrorCopyDescription(*error);char text[1024];if(CFStringGetCString(description,text,sizeof(text),kCFStringEncodingUTF8))fprintf(stderr,"fixture TLS verification failed: %s\n",text);CFRelease(description);}return ok;}
static OSStatus fixture_evaluate_old(SecTrustRef trust,SecTrustResultType *result) {anchor(trust);OSStatus status=SecTrustEvaluate(trust,result);fprintf(stderr,"fixture TLS result: %d/%u\n",status,(unsigned int)*result);return status;}
#define INTERPOSE(replacement,original) __attribute__((used)) static struct {const void *replacement;const void *original;} pair_##original __attribute__((section("__DATA,__interpose")))={(const void*)&replacement,(const void*)&original};
INTERPOSE(fixture_getaddrinfo,getaddrinfo)
INTERPOSE(fixture_connect,connect)
INTERPOSE(fixture_evaluate,SecTrustEvaluateWithError)
INTERPOSE(fixture_evaluate_old,SecTrustEvaluate)

INTERPOSE(fixture_endpoint,nw_endpoint_create_host)
INTERPOSE(fixture_fopen,fopen)

INTERPOSE(fixture_connectx,connectx)

static int fixture_open(const char *path,int flags,...) {
    mode_t mode=0;if(flags&O_CREAT){va_list args;va_start(args,flags);mode=va_arg(args,int);va_end(args);}
    if(strstr(path,"/sso/cache/"))fprintf(stderr,"fixture cache open: %s\n",path);
    return open(path,flags,mode);
}
INTERPOSE(fixture_open,open)
